//! Opening an index, and writing documents into it.
//!
//! # What an index stores
//!
//! Every index stores the metadata blob, live set, forward codes and caller
//! attributes. Binary indexes additionally store one inverted posting list per
//! dimension and the bit planes of `z = D - |x|`; model-bound residual indexes
//! need only forward rows for their exact scorer, so they omit those views.
//!
//! The last two arrived at M2 with the inverted kernel that reads them, not
//! before. M1 stored neither, because the forward path recovers a document's
//! weight from its own code, and storage nobody consults is storage that is
//! wrong the first time it drifts.
//!
//! # Why the complement weight rather than the weight
//!
//! The inverted path never sees a document's code, so it cannot recompute `|x|`
//! and must read it. Storing `z = D - |x|` rather than `|x|` makes Hamming's
//! ranking key `S = 2a + z` -- an **addition**, and one that reuses the same
//! ripple-carry the intersection count already needs. Storing `|x|` would make
//! it a subtraction and buy nothing.
//!
//! # Forward layout
//!
//! A document's code occupies `row_bits` consecutive ordinals starting at
//! `row * row_bits` within its block, where `row_bits` is the code width rounded
//! up to a word. Rows therefore never straddle a word, and `rows_per_block` is
//! chosen so a row never straddles a chunk. Both are the substrate's own
//! "the seam is paid at the boundary" rule: pay for alignment once, at the
//! layout, rather than in every kernel that reads it.

use crate::code::{CodeRef, DocId};
use crate::keyspace::{BLOCK_ORDINALS, KeySpace};
use crate::meta::IndexMeta;
use crate::store::{Batch, SetSnapshot, SetStore, Version};
use crate::{Error, Result};

/// What is known about every live document in one block.
///
/// # Stale statistics are a wrong answer, so they are invalidated, not refreshed
///
/// The search uses `w_min` to tighten a bound, and a bound that is too tight
/// **drops results silently** -- no crash, no count change, just a document
/// missing from the top-k. So a block's statistics are deleted by a live
/// code write or delete in the same atomic batch; an absent-ID delete and an
/// attribute-only change leave them intact. Pruning applies
/// only where statistics exist. A block with none is scored with the loose
/// bound: slower, never wrong.
///
/// That is the opposite of refreshing them on write, which would be faster and
/// would make correctness depend on the refresh being right every time.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct BlockStats {
    /// The smallest `|x|` among the block's live documents.
    pub w_min: u32,
    /// The largest.
    pub w_max: u32,
}

impl BlockStats {
    /// As ordinals under the block's `STAT` key: `w_min` then `w_max`, each in
    /// its own 32-ordinal window, plus a presence marker at 0 so an all-zero
    /// `w_min` is distinguishable from an absent entry.
    #[must_use]
    pub fn to_ordinals(self) -> Vec<u64> {
        let mut v = vec![0u64];
        for b in 0..32u64 {
            if self.w_min >> b & 1 == 1 {
                v.push(1 + b);
            }
            if self.w_max >> b & 1 == 1 {
                v.push(33 + b);
            }
        }
        v
    }

    /// Parse what [`BlockStats::to_ordinals`] wrote, or `None` if absent.
    #[must_use]
    pub fn from_ordinals(ords: &[u64]) -> Option<Self> {
        if ords.first() != Some(&0) {
            return None;
        }
        let mut s = Self { w_min: 0, w_max: 0 };
        for &o in &ords[1..] {
            match o {
                1..=32 => s.w_min |= 1 << (o - 1),
                33..=64 => s.w_max |= 1 << (o - 33),
                _ => return None,
            }
        }
        (s.w_min <= s.w_max).then_some(s)
    }
}

/// Where a document's forward row lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RowAddr {
    /// The forward block key's index.
    pub block: u64,
    /// The first ordinal of the row, absolute.
    pub base: u64,
}

/// An open index.
#[derive(Debug)]
pub struct Index<S: SetStore> {
    store: S,
    keys: KeySpace,
    meta: IndexMeta,
}

impl<S: SetStore> Index<S> {
    /// Create an index of `dims`-bit caller-defined codes in `namespace`,
    /// refusing to clobber one that already exists there.
    pub fn create(store: S, namespace: u8, dims: u32) -> Result<Self> {
        Self::create_with_model(store, namespace, dims, None)
    }

    /// Create an index whose rows require one external encoder/decoder model.
    ///
    /// The core treats the identity as opaque. A codec layer compares it before
    /// encoding or scoring so a different, well-formed model cannot silently
    /// interpret these rows.
    pub fn create_with_model_id(
        store: S,
        namespace: u8,
        dims: u32,
        model_id: [u8; 32],
    ) -> Result<Self> {
        Self::create_with_model(store, namespace, dims, Some(model_id))
    }

    fn create_with_model(
        store: S,
        namespace: u8,
        dims: u32,
        model_id: Option<[u8; 32]>,
    ) -> Result<Self> {
        if dims == 0 {
            return Err(Error::DimensionMismatch { want: 1, got: 0 });
        }
        // Two bounds, and the forward layout's is the one that binds. Dimensions
        // are numbered `0..dims`, so the largest index a `DIM` key carries is
        // `dims - 1` and must fit the key's 20-bit index field; and a row is
        // `row_bits` ordinals inside one block, so a code wider than a block
        // leaves `rows_per_block` at zero and `row_addr` divides by it.
        let by_key = u32::try_from(crate::KeySpace::INDEX_MAX + 1).unwrap_or(u32::MAX);
        let by_row = u32::try_from(BLOCK_ORDINALS).unwrap_or(u32::MAX);
        let max_dims = by_key.min(by_row);
        if dims > max_dims {
            return Err(Error::TooManyDimensions {
                dims,
                max: max_dims,
            });
        }
        let meta = model_id.map_or_else(
            || IndexMeta::new(dims),
            |identity| IndexMeta::with_model_id(dims, identity),
        );
        let keys = KeySpace::new(namespace);
        let snap = store.snapshot()?;
        if !snap.load(keys.meta())?.is_empty() {
            return Err(Error::AlreadyExists(namespace));
        }
        // Every defined kind is reserved, including currently optional views.
        // Checking META alone lets an application's LIVE or FWD key become
        // document data when an index is created in the same namespace.
        for kind in [
            crate::Kind::Meta,
            crate::Kind::Live,
            crate::Kind::Dim,
            crate::Kind::ZPlane,
            crate::Kind::Forward,
            crate::Kind::Stat,
            crate::Kind::Attr,
            crate::Kind::Compaction,
            crate::Kind::Remap,
        ] {
            let (lo, hi) = keys.kind_range(kind);
            if let Some(key) = snap.key_range(lo, hi)?.first().copied() {
                return Err(Error::KeyspaceOccupied { namespace, key });
            }
        }
        let mut b = Batch::new();
        for o in meta.to_ordinals() {
            b.insert(keys.meta(), o);
        }
        store.write(&b)?;
        Ok(Self { store, keys, meta })
    }

    /// Open an index previously created in `namespace`.
    pub fn open(store: S, namespace: u8) -> Result<Self> {
        let keys = KeySpace::new(namespace);
        let snap = store.snapshot()?;
        if snap.cardinality(keys.compaction())? != 0 {
            return Err(Error::CompactionPending(namespace));
        }
        let meta = IndexMeta::from_ordinals(&snap.load(keys.meta())?)?;
        Ok(Self { store, keys, meta })
    }

    /// The geometry this index was built with.
    #[must_use]
    pub fn meta(&self) -> IndexMeta {
        self.meta
    }

    /// This index's key space.
    #[must_use]
    pub fn keys(&self) -> KeySpace {
        self.keys
    }

    /// The underlying store.
    #[must_use]
    pub fn store(&self) -> &S {
        &self.store
    }

    /// The largest id this index can address.
    ///
    /// The same conservative bound applies to binary and residual layouts so
    /// compaction's ID mapping has one range to validate. Binary statistics
    /// currently set that bound even though residual rows omit STAT keys.
    ///
    /// **Two constraints, and the tighter one is not the obvious one.** A
    /// forward row starts at `block * BLOCK_ORDINALS + row * row_bits`, which
    /// must not overflow -- that was the only bound this returned at first, and
    /// it is around `2^58`. But a block's statistics live under `STAT` keyed by
    /// the **block number**, and a key's index field is 20 bits, so
    /// `block_of(id)` must be at most `KeySpace::INDEX_MAX` as well. That caps
    /// ids at `2^36 - 1`, four million times lower.
    ///
    /// Above it, `stat()` would have carried into the kind field: a debug panic
    /// since `KeySpace::key` gained its assertion, and before that a statistics
    /// key silently written under a kind belonging to something else. Found by
    /// checking the claim that the assertion is unreachable from the public API,
    /// which it was not.
    ///
    /// Roughly 68.7 billion documents, so this bounds nothing anyone will build
    /// -- the design's own largest case is `1e9`. It is here to be right rather
    /// than to bind.
    #[must_use]
    pub fn max_doc_id(&self) -> u64 {
        let per = u64::from(self.meta.rows_per_block);
        let by_row = ((u64::MAX - BLOCK_ORDINALS) / BLOCK_ORDINALS) * per + (per - 1);
        // The last ordinal whose block still fits the key's index field.
        let by_stat = ((crate::KeySpace::INDEX_MAX + 1) * BLOCK_ORDINALS) - 1;
        by_row.min(by_stat)
    }

    /// Where document `id`'s forward row begins.
    ///
    /// Callers must have checked the id against [`Index::max_doc_id`] first;
    /// above it this arithmetic overflows.
    #[must_use]
    pub fn row_addr(&self, id: DocId) -> RowAddr {
        let per = u64::from(self.meta.rows_per_block);
        let (block, row) = (id.get() / per, id.get() % per);
        RowAddr {
            block,
            base: block * BLOCK_ORDINALS + row * u64::from(self.meta.row_bits),
        }
    }

    /// Begin a write.
    #[must_use]
    pub fn writer(&self) -> Writer<'_, S> {
        Writer {
            index: self,
            batch: Batch::new(),
            snap: None,
            live_block: None,
            marked: std::collections::HashSet::new(),
            deleted: std::collections::HashSet::new(),
            fwd_block: None,
            scratch_row: Vec::new(),
        }
    }

    /// Recompute binary-index block statistics; residual indexes return their
    /// current version because they have no binary pruning statistics.
    ///
    /// Call after a binary bulk ingest. Until it runs, blocks simply have no
    /// statistics and are scored with the loose bound -- correct, and slower for
    /// the ratio metrics. Not maintained incrementally on purpose: see
    /// [`BlockStats`].
    pub fn refresh_stats(&self) -> Result<Version> {
        // Residual scoring has no weight bound. No ZPLANE or STAT keys exist in
        // its layout, and an empty write would only create an extra version.
        if self.meta.is_residual() {
            return Ok(self.store.snapshot()?.version());
        }
        let snap = self.store.snapshot()?;
        let last = snap
            .max(self.keys.live())?
            .map_or(0, crate::keyspace::block_of);
        let mut batch = Batch::new();
        let mut live = crate::slice::zero_mask();
        let mut offs = Vec::new();

        for block in 0..=last {
            if !snap.load_block(self.keys.live(), block, &mut live)? {
                continue;
            }
            crate::slice::offsets(&live, &mut offs);
            let mut w_min = u32::MAX;
            let mut w_max = 0u32;
            for &off in &offs {
                let id = DocId(block * BLOCK_ORDINALS + u64::from(off));
                let mut z = 0u32;
                for j in 0..self.meta.z_planes() {
                    if snap.contains(self.keys.zplane(j), id.get())? {
                        z |= 1 << j;
                    }
                }
                let w = self.meta.dims - z;
                w_min = w_min.min(w);
                w_max = w_max.max(w);
            }
            if w_min <= w_max {
                let stats = BlockStats { w_min, w_max };
                batch.delete_key(self.keys.stat(block));
                for o in stats.to_ordinals() {
                    batch.insert(self.keys.stat(block), o);
                }
            }
        }
        self.store.write(&batch)
    }

    /// Count blocks with live documents without depending on binary statistics.
    pub fn live_blocks(&self) -> Result<u64> {
        let snap = self.store.snapshot()?;
        let Some(last) = snap.max(self.keys.live())? else {
            return Ok(0);
        };
        let mut mask = crate::slice::zero_mask();
        let mut count = 0;
        for block in 0..=crate::keyspace::block_of(last) {
            if snap.load_block(self.keys.live(), block, &mut mask)? {
                count += 1;
            }
        }
        Ok(count)
    }

    /// How many documents are live.
    pub fn len(&self) -> Result<u64> {
        self.store.snapshot()?.cardinality(self.keys.live())
    }

    /// Whether the index holds no live documents.
    pub fn is_empty(&self) -> Result<bool> {
        Ok(self.len()? == 0)
    }
}

/// Accumulates a set of changes and commits them as one atomic batch.
///
/// **One batch, deliberately.** A document's forward bits, its liveness and its
/// attributes commit together, so no snapshot ever sees a document that is live
/// but has no code, or has a code and is not live. The first scores garbage; the
/// second is invisible. Neither fails loudly.
pub struct Writer<'i, S: SetStore> {
    index: &'i Index<S>,
    batch: Batch,
    /// A read view taken lazily when a put or delete needs to check whether an
    /// id is already live.
    snap: Option<S::Snap>,
    /// The live-set mask for one block, and which block it is.
    ///
    /// A bulk ingest writes dense ascending ids, so this is loaded once per
    /// 65 536 documents and every membership test after that is a bit test.
    /// Probing the store per document instead would cost a tree descent each
    /// time and give most of the saving back.
    live_block: Option<(u64, crate::slice::BlockMask)>,
    /// Ids this batch has put or deleted, distinct from ids only in the store.
    ///
    /// A later `put` of one must not difference against the store's old row:
    /// neither a prior put nor a pending delete is visible to that snapshot.
    /// It clears the whole row before writing its new value.
    ///
    /// **A set of ids, not a per-block mask, and that is a bug fix.** It was
    /// `Option<(u64, BlockMask)>` -- one block, replaced whenever a `put`
    /// reached a different one. A batch that wrote a block, then another, then
    /// returned to the first forgot the first block's marks, so the re-put took
    /// the differencing path and differenced against the *stored* row rather
    /// than against what the batch had already written. The result was a
    /// silently wrong row: `put(0, a); put(70_000, b); put(0, c)` left doc 0
    /// holding `a | c`, weight 6 where the last code has 3.
    ///
    /// The single-block form was never wrong for ascending ingest, which never
    /// revisits a block, and that is every ingest path here -- which is why it
    /// survived. It is wrong for an update batch keyed by arbitrary ids, which
    /// is a call pattern the API permits and the gRPC ingest path passes
    /// straight through.
    ///
    /// Bounded by `flush_if_large`, and cleared there: once a prefix is
    /// committed the store holds those rows, so differencing against it is both
    /// valid and cheaper than clearing blindly.
    marked: std::collections::HashSet<u64>,
    /// IDs whose latest mutation in this batch is a delete. A repeated delete
    /// is a no-op even though the snapshot still sees the old live row.
    deleted: std::collections::HashSet<u64>,
    /// One forward block's bits, for reading a document's previous row.
    fwd_block: Option<(u64, crate::slice::BlockMask)>,
    /// The row being written, materialized once so dense and sparse codes take
    /// the same path through the difference.
    scratch_row: Vec<u64>,
}

impl<S: SetStore> std::fmt::Debug for Writer<'_, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Writer")
            .field("pending", &self.batch.len())
            .finish_non_exhaustive()
    }
}

impl<S: SetStore> Writer<'_, S> {
    /// May this id need a full clear before a `put`?
    ///
    /// A prior mutation in this batch returns true conservatively, including
    /// a delete: the snapshot cannot see pending operations and is unsafe for
    /// differencing. Otherwise the store's live mask answers whether a row
    /// exists. The answer decides whether a `put` must clear previous state.
    /// For a **fresh** id there is nothing to clear, and clearing anyway costs
    /// `dims + row_bits` operations that do nothing -- two thirds of the
    /// 784 per document measured at D=256 before this existed.
    fn is_present(&mut self, id: DocId) -> Result<bool> {
        // A prior put or delete makes the snapshot stale for this id.
        // `live_block` holds one block and `mark_present` patches only that one,
        // so a fresh id marked in one block was forgotten the moment a `put`
        // reached another -- the re-put then read "not present", cleared
        // nothing, and left the two codes OR-ed together. Same single-block
        // assumption as the `marked` field above, same failure, one layer down.
        if self.marked.contains(&id.get()) {
            return Ok(true);
        }
        let block = crate::keyspace::block_of(id.get());
        if self.live_block.as_ref().is_none_or(|(b, _)| *b != block) {
            if self.snap.is_none() {
                self.snap = Some(self.index.store.snapshot()?);
            }
            let mut mask = crate::slice::zero_mask();
            self.snap.as_ref().expect("just populated").load_block(
                self.index.keys.live(),
                block,
                &mut mask,
            )?;
            self.live_block = Some((block, mask));
        }
        let (_, mask) = self.live_block.as_ref().expect("just populated");
        let off = (id.get() % BLOCK_ORDINALS) as usize;
        Ok(mask[off >> 6] >> (off & 63) & 1 == 1)
    }

    /// Record that an id is now present, so a second `put` of it in this same
    /// batch clears what the first wrote. The snapshot predates the batch and
    /// cannot see it, which is the case this exists for.
    fn mark_present(&mut self, id: DocId) {
        self.deleted.remove(&id.get());
        let block = crate::keyspace::block_of(id.get());
        let off = (id.get() % BLOCK_ORDINALS) as usize;
        // Patch the cached live mask **only when it is this id's block**. The
        // block was previously discarded here, so marking an id in one block
        // set that offset in whatever other block happened to be cached -- a
        // false "present" for an unrelated id at the same offset. That cost a
        // pointless stored-row read rather than a wrong answer, and only
        // because `is_present` now consults `marked` before this cache.
        if let Some((b, mask)) = self.live_block.as_mut()
            && *b == block
        {
            mask[off >> 6] |= 1u64 << (off & 63);
        }
        self.marked.insert(id.get());
    }

    /// Did this batch already touch the id, making the stored row stale?
    fn touched_in_batch(&self, id: DocId) -> bool {
        self.marked.contains(&id.get())
    }

    /// The document's currently stored row, as `row_bits / 64` words.
    ///
    /// Cached per forward block, which holds `rows_per_block` documents, so a
    /// scan of ascending ids reads each block once.
    fn stored_row(&mut self, addr: RowAddr) -> Result<&[u64]> {
        if self
            .fwd_block
            .as_ref()
            .is_none_or(|(b, _)| *b != addr.block)
        {
            if self.snap.is_none() {
                self.snap = Some(self.index.store.snapshot()?);
            }
            let mut mask = crate::slice::zero_mask();
            self.snap.as_ref().expect("just populated").load_block(
                self.index.keys.forward(),
                addr.block,
                &mut mask,
            )?;
            self.fwd_block = Some((addr.block, mask));
        }
        let (_, mask) = self.fwd_block.as_ref().expect("just populated");
        let lo = (addr.base % BLOCK_ORDINALS) as usize / 64;
        let words = (self.index.meta.row_bits / 64) as usize;
        Ok(&mask[lo..lo + words])
    }

    /// Insert or replace a document's code.
    pub fn put(&mut self, id: DocId, code: CodeRef<'_>) -> Result<&mut Self> {
        let meta = self.index.meta;
        let width = match code {
            CodeRef::Dense(w) => (w.len() as u32) * 64,
            CodeRef::Sparse(p) => p.last().map_or(0, |b| b + 1),
        };
        if width > meta.row_bits {
            return Err(Error::DimensionMismatch {
                want: meta.dims,
                got: width,
            });
        }
        // The only place a caller-supplied id enters. `DocId`'s field is public,
        // so `DocId::new` cannot be relied on; above this bound `row_addr`
        // overflows, which panicked in debug and silently wrapped in release.
        if id.get() > self.index.max_doc_id() {
            return Err(Error::DocIdTooLarge {
                id: id.get(),
                dims: meta.dims,
                max: self.index.max_doc_id(),
            });
        }
        let addr = self.index.row_addr(id);
        let base = addr.base;
        let keys = self.index.keys;
        let binary_views = !meta.is_residual();
        // Replace, not merge, in every view this layout stores -- but only if
        // there is
        // something to replace**. Clearing an id that holds nothing costs
        // `dims + row_bits` operations that do nothing, which is every document
        // of a bulk ingest.
        //
        // **A replacement writes only the difference.** The comment that stood
        // here said the old code "is not known without reading it back, so
        // clearing is `O( dims + row_bits )` and cannot be narrowed" -- true
        // about the first clause and a non-sequitur about the second. Reading it
        // back costs one block read per `rows_per_block` documents, and buys the
        // difference: an update emits operations only for bits that actually
        // changed, and re-putting an identical code emits none at all.
        //
        // That is a query-latency change as much as an ingest one. The storage
        // layer holds its memtable lock across a whole batch by construction --
        // releasing it per record would let a reader observe half a commit -- so
        // every operation not emitted is exclusion a concurrent reader does not
        // wait through.
        //
        // Measured, worst-case reader stall at 262 144 documents with a
        // concurrent writer and no checkpoints running:
        //
        // ```text
        // batch    before   same code   changed
        //   500   27.17ms      2.43ms    7.78ms
        //  5000  121.97ms      5.09ms   24.15ms
        // 20000  485.96ms      8.97ms   95.05ms
        // ```
        //
        // **`changed` is the number, about 5x.** `same code` is re-putting a
        // document's own code -- the idempotent resume path -- where the
        // difference is empty by construction and the gain is 24x to 54x. The
        // figure that first motivated this ( "an overwrite holds 12x to 39x
        // longer than a fresh insert" ) was measured entirely in that degenerate
        // case, which is why it is not quoted here.
        let present = self.is_present(id)?;
        // A prior put or delete of this id in the batch is invisible to the
        // snapshot. Differencing against its old row would lose pending writes
        // or turn delete-then-readd into an empty row, so clear before writing.
        let diffable = present && !self.touched_in_batch(id);
        if present && !diffable {
            if binary_views {
                for d in 0..meta.dims {
                    self.batch.remove(keys.dim(d), id.get());
                }
                for j in 0..meta.z_planes() {
                    self.batch.remove(keys.zplane(j), id.get());
                }
            }
            // One operation, not `row_bits` of them. A row is contiguous by
            // construction -- it starts at `base` and runs `row_bits` ordinals
            // -- so the storage layer's range form applies exactly, and it is
            // inclusive at the top where this loop was exclusive.
            self.batch
                .remove_range(keys.forward(), base, base + u64::from(meta.row_bits) - 1);
        }

        if diffable {
            let words = (meta.row_bits / 64) as usize;
            self.scratch_row.clear();
            self.scratch_row.resize(words, 0);
            match code {
                CodeRef::Dense(w) => {
                    for (i, x) in w.iter().take(words).enumerate() {
                        self.scratch_row[i] = *x;
                    }
                }
                CodeRef::Sparse(pos) => {
                    for &b in pos {
                        if b < meta.dims {
                            self.scratch_row[(b as usize) >> 6] |= 1u64 << (b & 63);
                        }
                    }
                }
            }
            // Bits at or above `dims` are never written, so they must not be
            // compared either: a stored row is zero there and a dense code may
            // not be.
            if !meta.dims.is_multiple_of(64) {
                let last = (meta.dims as usize) / 64;
                if last < words {
                    self.scratch_row[last] &= (1u64 << (meta.dims % 64)) - 1;
                    for w in self.scratch_row.iter_mut().skip(last + 1) {
                        *w = 0;
                    }
                }
            }

            let old: Vec<u64> = self.stored_row(addr)?.to_vec();
            let (mut old_weight, mut new_weight) = (0u32, 0u32);
            for (i, (o, n)) in old.iter().zip(self.scratch_row.iter()).enumerate() {
                if binary_views {
                    old_weight += o.count_ones();
                    new_weight += n.count_ones();
                }
                let base_bit = (i as u32) * 64;
                let mut cleared = o & !n;
                while cleared != 0 {
                    let b = base_bit + cleared.trailing_zeros();
                    cleared &= cleared - 1;
                    if binary_views {
                        self.batch.remove(keys.dim(b), id.get());
                    }
                    self.batch.remove(keys.forward(), base + u64::from(b));
                }
                let mut added = n & !o;
                while added != 0 {
                    let b = base_bit + added.trailing_zeros();
                    added &= added - 1;
                    if binary_views {
                        self.batch.insert(keys.dim(b), id.get());
                    }
                    self.batch.insert(keys.forward(), base + u64::from(b));
                }
            }
            // Weight planes move only where the complement's bits differ.
            if binary_views {
                let (old_z, new_z) = (meta.dims - old_weight, meta.dims - new_weight);
                for j in 0..meta.z_planes() {
                    let (o, n) = (old_z >> j & 1, new_z >> j & 1);
                    if o == n {
                        continue;
                    }
                    if n == 1 {
                        self.batch.insert(keys.zplane(j), id.get());
                    } else {
                        self.batch.remove(keys.zplane(j), id.get());
                    }
                }
                self.batch
                    .delete_key(keys.stat(crate::keyspace::block_of(id.get())));
            }
            self.batch.insert(keys.live(), id.get());
            self.mark_present(id);
            return Ok(self);
        }
        let mut set_bit = |b: u32| {
            if b < meta.dims {
                self.batch.insert(keys.forward(), base + u64::from(b));
            }
        };
        match code {
            CodeRef::Dense(words) => {
                for (wi, word) in words.iter().enumerate() {
                    let mut x = *word;
                    while x != 0 {
                        set_bit((wi as u32) * 64 + x.trailing_zeros());
                        x &= x - 1;
                    }
                }
            }
            CodeRef::Sparse(pos) => {
                for &p in pos {
                    set_bit(p);
                }
            }
        }
        if binary_views {
            // Binary search reads the inverted and complement-weight views.
            // A residual row has no reader for either one.
            let mut weight = 0u32;
            let mut set_dim = |b: u32| {
                if b < meta.dims {
                    weight += 1;
                    self.batch.insert(keys.dim(b), id.get());
                }
            };
            match code {
                CodeRef::Dense(words) => {
                    for (wi, word) in words.iter().enumerate() {
                        let mut x = *word;
                        while x != 0 {
                            set_dim((wi as u32) * 64 + x.trailing_zeros());
                            x &= x - 1;
                        }
                    }
                }
                CodeRef::Sparse(pos) => {
                    for &p in pos {
                        set_dim(p);
                    }
                }
            }
            let z = meta.dims - weight;
            for j in 0..meta.z_planes() {
                if z >> j & 1 == 1 {
                    self.batch.insert(keys.zplane(j), id.get());
                }
            }
            // Invalidating bounds belongs in the same commit as the row.
            self.batch
                .delete_key(keys.stat(crate::keyspace::block_of(id.get())));
        }
        self.batch.insert(keys.live(), id.get());
        self.mark_present(id);
        Ok(self)
    }

    /// Pack a bounded run of fresh, ascending 512-bit residual rows by chunk.
    ///
    /// Returns `false` without changing the batch when a row is already live,
    /// was touched earlier in this batch, or the run does not start at a chunk
    /// boundary and cover at least one full chunk of consecutive IDs.
    /// The caller then uses [`Self::put`] for the entire run, preserving its
    /// overwrite, delete/readd, and repeated-ID semantics. The residual codec
    /// stores exactly eight words per document; one 65,536-bit forward chunk
    /// holds 128 such rows. The batch keeps each chunk patch with its LIVE and
    /// ATTR operations, and the yesnodb adapter logs native container masks.
    pub fn try_put_residual_tile(&mut self, rows: &[(DocId, [u64; 8])]) -> Result<bool> {
        if !self.index.store.supports_chunk_patch() {
            return Ok(false);
        }
        if !self.index.meta.is_residual()
            || self.index.meta.dims != 512
            || rows.len() < self.index.meta.rows_per_block as usize
            || !rows[0]
                .0
                .get()
                .is_multiple_of(u64::from(self.index.meta.rows_per_block))
        {
            return Ok(false);
        }
        let mut previous = None;
        for &(id, _) in rows {
            if id.get() > self.index.max_doc_id() {
                return Err(Error::DocIdTooLarge {
                    id: id.get(),
                    dims: self.index.meta.dims,
                    max: self.index.max_doc_id(),
                });
            }
            if previous.is_some_and(|last| id.get() != last + 1) || self.is_present(id)? {
                return Ok(false);
            }
            previous = Some(id.get());
        }

        let mut prefix = None;
        let mut words = vec![0u64; crate::slice::BLOCK_WORDS];
        let forward = self.index.keys.forward();
        for &(id, code) in rows {
            let addr = self.index.row_addr(id);
            if prefix != Some(addr.block) {
                if let Some(old_prefix) = prefix {
                    let image =
                        std::mem::replace(&mut words, vec![0u64; crate::slice::BLOCK_WORDS]);
                    let card: u32 = image.iter().map(|word| word.count_ones()).sum();
                    if card != 0 {
                        self.batch.patch_chunk(
                            forward,
                            old_prefix,
                            yesno_core::Container::new_array(),
                            yesno_core::Container::Bitmap(
                                yesno_core::container::BitmapContainer::from_words(image, card),
                            ),
                        );
                    }
                }
                prefix = Some(addr.block);
            }
            let word_offset = ((addr.base % BLOCK_ORDINALS) / 64) as usize;
            words[word_offset..word_offset + code.len()].copy_from_slice(&code);
            self.batch.insert(self.index.keys.live(), id.get());
            self.mark_present(id);
        }
        if let Some(last_prefix) = prefix {
            let card: u32 = words.iter().map(|word| word.count_ones()).sum();
            if card != 0 {
                self.batch.patch_chunk(
                    forward,
                    last_prefix,
                    yesno_core::Container::new_array(),
                    yesno_core::Container::Bitmap(
                        yesno_core::container::BitmapContainer::from_words(words, card),
                    ),
                );
            }
        }
        Ok(true)
    }

    /// Remove a document.
    ///
    /// The ordinal is retired, not recycled. A reused ordinal whose old forward
    /// bits survived anywhere would score as the new document with the old
    /// code -- silently, with the right cardinality. Recycling happens only in a
    /// compaction that rewrites every key for the block. Deleting an absent
    /// id is a no-op, including for that block's pruning statistics.
    pub fn delete(&mut self, id: DocId) -> Result<&mut Self> {
        // A delete after a pending delete, or for an ID never live in the
        // snapshot, must not erase this block's valid statistics.
        if self.deleted.contains(&id.get())
            || (!self.marked.contains(&id.get()) && !self.is_present(id)?)
        {
            return Ok(self);
        }
        let RowAddr { base, .. } = self.index.row_addr(id);
        let keys = self.index.keys;
        // The same contiguous row, and the larger of the two wins: a delete
        // emitted `row_bits` removes here, about half of its operations at
        // D=256.
        self.batch.remove_range(
            keys.forward(),
            base,
            base + u64::from(self.index.meta.row_bits) - 1,
        );
        if !self.index.meta.is_residual() {
            for d in 0..self.index.meta.dims {
                self.batch.remove(keys.dim(d), id.get());
            }
            for j in 0..self.index.meta.z_planes() {
                self.batch.remove(keys.zplane(j), id.get());
            }
            self.batch
                .delete_key(keys.stat(crate::keyspace::block_of(id.get())));
        }
        self.batch.remove(keys.live(), id.get());
        // A later put in this batch must clear and rewrite the full row: the
        // snapshot still sees the pre-delete code. Keep the live cache aligned
        // with the pending mutation for a later writer flush.
        let block = crate::keyspace::block_of(id.get());
        if let Some((cached_block, mask)) = self.live_block.as_mut()
            && *cached_block == block
        {
            let offset = (id.get() % BLOCK_ORDINALS) as usize;
            mask[offset >> 6] &= !(1u64 << (offset & 63));
        }
        self.marked.insert(id.get());
        self.deleted.insert(id.get());
        Ok(self)
    }

    /// Attach a boolean attribute, usable directly in a filter.
    ///
    /// Fallible for the same reason `put` is: `term` is caller-supplied and is
    /// packed into the key's 20-bit index field, so an out-of-range one used to
    /// carry into the kind field silently. It is refused here rather than
    /// asserted about deeper down, where the only report available is a panic on
    /// a caller's input.
    pub fn attr(&mut self, id: DocId, term: u32) -> Result<&mut Self> {
        crate::KeySpace::check_term(term)?;
        self.batch.insert(self.index.keys.attr(term), id.get());
        Ok(self)
    }

    /// Detach an attribute.
    ///
    /// Refuses the same terms `attr` refuses, so that a caller cannot use one
    /// half of the pair on a value the other half rejects.
    pub fn unattr(&mut self, id: DocId, term: u32) -> Result<&mut Self> {
        crate::KeySpace::check_term(term)?;
        self.batch.remove(self.index.keys.attr(term), id.get());
        Ok(self)
    }

    /// How many operations are pending.
    #[must_use]
    pub fn pending(&self) -> usize {
        self.batch.len()
    }

    /// Commit everything accumulated, atomically.
    pub fn commit(mut self) -> Result<Version> {
        // Group by key first. Ingest emits document-major -- one document
        // touches one key per set bit -- and a store applies a batch in order,
        // which makes that the worst possible order.
        //
        // **Still worth keeping, but barely, and the margin has moved twice.**
        // Three runs per arm, 262 144 documents, commit time:
        //
        //   with this      6.0 - 6.3 s
        //   without it     6.9 - 7.6 s     ~15%, ranges disjoint
        //
        // It was 2.8x that morning ( 5.5 s against 16.4 s ). yesnodb's own
        // sort then moved from a pointer array to `(key, index)` pairs, which
        // captured nearly all of it. What is left is that sorting here lets
        // their `keys_ascending` skip their sort entirely, so exactly one sort
        // happens either way and ours is over an owned vector we already hold.
        //
        // Re-measure this rather than trusting the number above: it has been
        // stale twice in one day, and a margin this small is one upstream
        // change away from being zero.
        self.batch.group_by_key();
        self.index.store.write(&self.batch)
    }

    /// Commit and keep writing, if the batch has grown past `max_ops`.
    ///
    /// # What this gives up, and what it does not
    ///
    /// The atomicity that matters is **per document**: a document's code, its
    /// liveness and its attributes must land together, or a reader sees a live
    /// document with no code. That is a property of the operations for one
    /// document, not of a whole stream, so flushing between documents preserves
    /// it exactly.
    ///
    /// What is given up is all-or-nothing for the *stream*. A bulk ingest that
    /// fails midway leaves a committed prefix rather than nothing, and the
    /// caller resumes by re-sending the remainder -- which is safe, because
    /// `put` is idempotent for a given id and code.
    ///
    /// Without this, a stream accumulates one unbounded in-memory batch:
    /// measured at ~3 GiB for 262 144 documents, which exhausts memory long
    /// before ten million.
    ///
    /// Returns the version committed, or `None` if the batch was left alone.
    pub fn flush_if_large(&mut self, max_ops: usize) -> Result<Option<Version>> {
        if self.batch.len() < max_ops {
            return Ok(None);
        }
        let mut batch = std::mem::take(&mut self.batch);
        batch.group_by_key();
        let v = self.index.store.write(&batch)?;
        // The snapshot is now stale with respect to what was just committed,
        // but the cached live mask is not: puts set and deletes clear their
        // bits as mutations enter this writer. Dropping the snapshot forces a fresh one
        // only when a later `put` reaches a block this one has not seen.
        self.snap = None;
        // The forward cache contains rows read before this commit. A later
        // overwrite may now difference against a row the batch just changed,
        // so force that block to be read from the new snapshot.
        self.fwd_block = None;
        // The store now holds the batch's puts and deletes. These ids are no
        // longer invisible to a fresh snapshot, so later puts can difference
        // against it again.
        self.marked.clear();
        self.deleted.clear();
        Ok(Some(v))
    }
}
