//! The storage abstraction, and what it deliberately does not abstract.
//!
//! # Two tiers, and the boundary between them is a design statement
//!
//! Tier one -- [`SetStore`] and [`SetSnapshot`] -- is round-trip-bounded: every
//! method is one call answering one question. Tier two is the chunk-level
//! cursor used by scoring. The embedded adapter borrows storage containers;
//! the served yesnod peer receives blocks through a batched shared arena ( or
//! bounded inline frames ) on a pinned snapshot. A remote ordinal-streaming
//! Flight implementation of tier two was prohibitively expensive in its
//! measured frame; that finding does not measure the shared-arena channel.
//!
//! # `load` materializes, and is not what the scan uses
//!
//! [`SetSnapshot::load`] returns an owned `Vec<u64>`, which is the right shape
//! for a whole-key read and the wrong one for scanning: it costs an allocation
//! per chunk whether or not the caller wants the ordinals. The scan therefore
//! goes through [`SetSnapshot::load_block`] and the lane cursor above it, which
//! read one container into a reused mask and never build an ordinal list.
//! Forward scoring instead visits a borrowed word image where available, so
//! sparse candidates do not copy a whole bitmap merely to read one row. The
//! inverted tiled scorer needs all lane images at once; `Lanes::with_blocks`
//! lends them together or fills reused caller-owned masks as a fallback.
//!
//! This block used to say that shape **would** arrive at M2. It did, and the
//! allocation tests arrived with it: scoring is constant in the corpus -- 13
//! allocations at 20 documents against 16 at 131 092 -- and a counting query
//! allocates nothing at all. Those budgets are what stop the fast path quietly
//! decaying back into this one, which it has done once already.
//!
//! # Writes remain atomic across representations
//!
//! A residual ingest tile carries native forward chunk masks in [`Batch`]
//! beside its LIVE and ATTR changes. This is still one ordered transaction;
//! exposing the mask here lets the embedded adapter use yesnodb's compact WAL
//! patch while the in-memory store applies the same set algebra as an oracle.
//! The rare masks live behind a box: the width of this enum is paid by every
//! point insert, including binary indexes that never use chunk patches.

use crate::Result;
use yesno_core::Container;

/// A committed version of the store.
pub type Version = u64;

/// One atomic unit of change.
///
/// Every write a document needs -- its forward bits, its liveness, its
/// attributes -- must land in **one** batch. yesnodb commits a batch atomically
/// across shards, so a snapshot never observes half a document. Split them and a
/// reader can see a document whose forward code is present and whose liveness is
/// not, which scores as a real document with a wrong code rather than failing.
#[derive(Default, Debug, Clone)]
pub struct Batch {
    ops: Vec<Op>,
}

/// One change within a [`Batch`].
///
/// **Deliberately exhaustive.** Marking this `#[non_exhaustive]` would force
/// every store to carry a wildcard arm, which turns a future variant into an
/// operation that silently does nothing in every implementation. Closed, adding
/// one is a compile error in each store -- which is the outcome worth having,
/// since a store that cannot apply an operation must not appear to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    /// Add an ordinal to a key.
    Insert(u64, u64),
    /// Remove an ordinal from a key.
    Remove(u64, u64),
    /// Drop a key entirely.
    DeleteKey(u64),
    /// Remove every ordinal in `[lo, hi]`, inclusive, from a key.
    ///
    /// A whole forward row is `row_bits` consecutive ordinals, and clearing it
    /// one at a time costs the storage layer a WAL record and a copy-on-write
    /// clone **per ordinal** -- upstream's own reason for offering the range
    /// form. At D=256 a delete emitted 256 removes for the row, about half its
    /// operations, and a commit's cost tracks operations touched rather than
    /// work done, because each one pays a tree descent under the exclusive lock
    /// a concurrent reader waits on.
    ///
    /// Inclusive at both ends, matching the storage layer, so that the two
    /// cannot disagree at the boundary when this is translated.
    RemoveRange(u64, u64, u64),
    /// Apply `( old \ clear ) union set` to one 65,536-ordinal chunk.
    ///
    /// The two masks and the document's LIVE/ATTR operations remain in this
    /// batch, so readers never observe a code without its liveness.
    PatchChunk(u64, u64, Box<(Container, Container)>),
}

impl Op {
    /// The key this operation touches.
    #[must_use]
    pub fn key(&self) -> u64 {
        match self {
            Self::Insert(k, _)
            | Self::Remove(k, _)
            | Self::DeleteKey(k)
            | Self::RemoveRange(k, _, _)
            | Self::PatchChunk(k, _, _) => *k,
        }
    }
}

impl Batch {
    /// An empty batch.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add `ordinal` to `key`.
    pub fn insert(&mut self, key: u64, ordinal: u64) -> &mut Self {
        self.ops.push(Op::Insert(key, ordinal));
        self
    }

    /// Remove `ordinal` from `key`.
    pub fn remove(&mut self, key: u64, ordinal: u64) -> &mut Self {
        self.ops.push(Op::Remove(key, ordinal));
        self
    }

    /// Remove every ordinal in `[lo, hi]`, inclusive, from `key`.
    ///
    /// Nothing is recorded when `lo > hi`, so an empty span is not an operation.
    pub fn remove_range(&mut self, key: u64, lo: u64, hi: u64) -> &mut Self {
        if lo <= hi {
            self.ops.push(Op::RemoveRange(key, lo, hi));
        }
        self
    }

    /// Drop `key` entirely.
    pub fn delete_key(&mut self, key: u64) -> &mut Self {
        self.ops.push(Op::DeleteKey(key));
        self
    }

    /// Patch one native chunk atomically with the other operations in this batch.
    pub fn patch_chunk(
        &mut self,
        key: u64,
        prefix: u64,
        clear: Container,
        set: Container,
    ) -> &mut Self {
        if !clear.is_empty() || !set.is_empty() {
            self.ops
                .push(Op::PatchChunk(key, prefix, Box::new((clear, set))));
        }
        self
    }

    /// How many operations are pending.
    #[must_use]
    pub fn len(&self) -> usize {
        self.ops.len()
    }

    /// Whether the batch would change nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }

    /// The operations, in the order they were recorded.
    ///
    /// Order matters: a `DeleteKey` followed by inserts is a whole-key replace,
    /// and reordering it would silently become a merge.
    #[must_use]
    pub fn ops(&self) -> &[Op] {
        &self.ops
    }

    /// Group the operations by key, preserving their order **within** each key.
    ///
    /// # Why this is worth a pass over the batch
    ///
    /// A store applies a batch in order, and touching a different key on every
    /// operation is far more expensive than finishing one key before moving to
    /// the next. Measured against yesnodb with 8.4 million inserts over 256
    /// keys: **554 ms key-major against 7 712 ms document-major**, the same
    /// operations in a different order. haiiie's ingest is naturally
    /// document-major -- one document touches one key per set bit -- so it was
    /// paying that 14x until this existed.
    ///
    /// # When it is safe, which is not always
    ///
    /// Only when operations on **different** keys are independent. That holds
    /// for everything `Writer` emits: a document's writes to dimension `d` and
    /// dimension `e` do not interact, and a stable sort keeps each key's own
    /// sequence intact, so a `DeleteKey` still precedes the inserts that follow
    /// it and a remove-then-insert replacement still replaces.
    ///
    /// It does **not** hold in general. A caller building a batch by hand whose
    /// meaning depends on interleaving across keys must not call this, which is
    /// why it is an explicit method rather than something `commit` does.
    pub fn group_by_key(&mut self) {
        self.ops.sort_by_key(Op::key);
    }
}

/// A store of `u64 key -> set of u64 ordinals`.
pub trait SetStore: Send + Sync {
    /// A consistent read view.
    type Snap: SetSnapshot;

    /// Take a snapshot. Reads through it see one consistent version.
    fn snapshot(&self) -> Result<Self::Snap>;

    /// Apply a batch atomically.
    fn write(&self, batch: &Batch) -> Result<Version>;

    /// Whether this store can commit native chunk patches in an ordered batch.
    ///
    /// A caller may fall back to point operations without changing results.
    /// The default is conservative: a new store must opt in after its write
    /// transport proves the operation is atomic with the rest of the batch.
    fn supports_chunk_patch(&self) -> bool {
        false
    }

    /// Make everything written so far durable. A no-op for stores that have no
    /// durability to offer, which is why it is not named `checkpoint`.
    fn flush(&self) -> Result<()> {
        Ok(())
    }
}

/// A consistent read view of a [`SetStore`].
pub trait SetSnapshot: Send + Sync {
    /// `Sync` is required so a parallel scan can share **one** read view across
    /// workers. Giving each worker its own snapshot would be easier and wrong:
    /// they could land on different versions and the result would be a mixture
    /// no single version ever held.
    ///
    /// The store version this view reads.
    ///
    /// Reported back to the caller so a scan that resumed after an eviction can
    /// say which versions its answer spans.
    fn version(&self) -> Version;

    /// Every ordinal under `key`, ascending. Absent keys are empty, not an error.
    fn load(&self, key: u64) -> Result<Vec<u64>>;

    /// Occupied keys in the half-open range `[lo, hi)`, in ascending order.
    /// Creation uses this to refuse foreign data in any index-owned key kind.
    fn key_range(&self, lo: u64, hi: u64) -> Result<Vec<u64>>;

    /// How many ordinals `key` holds.
    fn cardinality(&self, key: u64) -> Result<u64>;

    /// Whether `key` holds `ordinal`.
    fn contains(&self, key: u64, ordinal: u64) -> Result<bool>;

    /// The largest ordinal under `key`, or `None` if it holds nothing.
    ///
    /// The scan needs to know how far the ordinal space extends without
    /// materializing it. The default is `O(|key|)` through `load`; a store that
    /// can answer from an index should override it, and yesnodb can.
    fn max(&self, key: u64) -> Result<Option<u64>> {
        Ok(self.load(key)?.last().copied())
    }

    /// Fill `out` with `key`'s membership bits within one 65 536-ordinal block.
    ///
    /// Returns whether anything was set. `out` is always fully written, so a
    /// caller may reuse one buffer across every key and every block.
    ///
    /// # This signature is the point, and the default implementation is not
    ///
    /// Taking a caller-owned buffer is what lets the scan allocate `O(planes)`
    /// in total rather than `O(blocks * query width)`. The default below routes
    /// through [`SetSnapshot::load`] and therefore allocates per call: correct,
    /// and exactly the decay an allocation budget exists to catch. A store that
    /// can read a block without materializing an ordinal list should override
    /// it, and yesnodb can -- through its chunk containers, which is M3's work.
    fn load_block(&self, key: u64, block: u64, out: &mut crate::slice::BlockMask) -> Result<bool> {
        out.fill(0);
        let base = block * crate::keyspace::BLOCK_ORDINALS;
        let end = base + crate::keyspace::BLOCK_ORDINALS;
        let all = self.load(key)?;
        let lo = all.partition_point(|&o| o < base);
        let mut any = false;
        for &o in &all[lo..] {
            if o >= end {
                break;
            }
            let b = (o - base) as usize;
            out[b >> 6] |= 1u64 << (b & 63);
            any = true;
        }
        Ok(any)
    }

    /// A cursor over `keys`, to be read block by block in ascending order.
    ///
    /// # Why this exists, and why it returns an `Option`
    ///
    /// [`SetSnapshot::load_block`] is addressed by `( key, block )` and so has
    /// to locate the key on every call. For a store that reaches a block by
    /// opening a stream and seeking, that lookup is the dominant cost and it is
    /// paid `query width * blocks` times -- 3 776 times for a 118-bit query over
    /// two million documents -- to move bytes that take a fraction as long.
    /// Measured on the embedded store: 8.68 ms of reads against 2.27 ms for the
    /// same bytes through 118 streams opened once and advanced, a 3.8x
    /// difference that is entirely lookup. 2.27 ms is also what this machine's
    /// memory takes to deliver those bytes in that access pattern, so the
    /// streaming form is at the hardware floor and there is nothing further to
    /// win here.
    ///
    /// `None` means "no cursor, keep using `load_block`", which is the honest
    /// default: a store whose reads are already addressed lookups -- an
    /// in-memory map, a fault injector wrapping another store -- gains nothing
    /// from a cursor and should not be made to fake one. The caller branches on
    /// the `Option` once per read, which is a predictable branch against a
    /// microsecond of work.
    fn open_lanes(&self, _keys: &[u64]) -> Result<Option<Box<dyn Lanes>>> {
        Ok(None)
    }
}

/// Simultaneously visible block words for a fixed set of lanes.
///
/// A store may lend bitmap words while keeping the containers that own them
/// alive for the visit. Other container shapes expose an expanded mask. The
/// visitor must not retain any returned reference after the call.
pub trait BlockLanes {
    /// Number of lanes in this block view.
    fn len(&self) -> usize;

    /// One lane's complete 65,536-bit block image.
    fn lane(&self, index: usize) -> &crate::slice::BlockMask;

    /// A resolved slice of borrowed masks when the store can provide one.
    /// The tiled kernel can then index without a virtual call per lane/tile.
    fn as_refs(&self) -> Option<&[&crate::slice::BlockMask]> {
        None
    }

    /// Whether the view has no lanes.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl BlockLanes for Vec<crate::slice::BlockMask> {
    fn len(&self) -> usize {
        Vec::len(self)
    }

    fn lane(&self, index: usize) -> &crate::slice::BlockMask {
        &self[index]
    }
}

/// A cursor over a fixed list of keys, read one block at a time.
///
/// Lanes are identified by their index in the `keys` slice given to
/// [`SetSnapshot::open_lanes`]. Blocks should be presented in ascending order
/// per lane; an implementation must still answer correctly if they are not,
/// because a scan that resumes after an eviction redoes a block, and a cursor
/// that silently returned an empty mask for a repeated block would drop every
/// document in it without raising anything.
pub trait Lanes {
    /// Fill `out` with lane `lane`'s membership bits within `block`.
    ///
    /// The contract is [`SetSnapshot::load_block`]'s: `out` is always fully
    /// written, and the return value says whether anything was set.
    fn read(&mut self, lane: usize, block: u64, out: &mut crate::slice::BlockMask) -> Result<bool>;

    /// Visit one block's words, borrowing them when the store has a word image.
    ///
    /// Calls `visit` exactly once on success, including an all-zero mask for an
    /// absent block. The borrow lasts only for the call. Errors propagate before
    /// visiting; block ordering and snapshot semantics are the same as `read`.
    ///
    /// A forward query may need just one row of this block. Requiring an owned
    /// mask makes it copy 8 KiB to read 32 bytes at D=256. The default preserves
    /// compatibility with stores that cannot lend words; local bitmap-backed
    /// stores can override this without exposing their container representation.
    fn with_block(
        &mut self,
        lane: usize,
        block: u64,
        visit: &mut dyn FnMut(&crate::slice::BlockMask),
    ) -> Result<()> {
        let mut mask = crate::slice::zero_mask();
        self.read(lane, block, &mut mask)?;
        visit(&mask);
        Ok(())
    }

    /// Visit several lanes of one block while their word images coexist.
    ///
    /// The default fills caller-owned masks and reuses them across blocks.
    /// Implementations with refcounted bitmap containers can lend their words
    /// directly. `visit` runs only after every lane read succeeds, so an
    /// eviction halfway through cannot expose a partial accumulator.
    fn with_blocks(
        &mut self,
        lane_count: usize,
        block: u64,
        scratch: &mut Vec<crate::slice::BlockMask>,
        visit: &mut dyn FnMut(&dyn BlockLanes),
    ) -> Result<()> {
        scratch.resize_with(lane_count, crate::slice::zero_mask);
        for (lane, mask) in scratch.iter_mut().enumerate().take(lane_count) {
            self.read(lane, block, mask)?;
        }
        visit(scratch);
        Ok(())
    }
}
