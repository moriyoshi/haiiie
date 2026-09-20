//! Search: the forward paths, filter composition, and exact top-k.
//!
//! # Three paths, and the planner rules between them
//!
//! `Gather` reads only the rows a filter admits; `DenseScan` walks every live
//! row. Between those two the rule is simply which set is smaller -- their costs
//! are `|admitted| * row_bits` against `|live| * row_bits`, so there is no
//! constant to tune and inventing one would be a knob with no measurement.
//!
//! **Between forward and inverted there is a constant, and it is measured.**
//! `Gather` reads whole rows, so its cost does not depend on the query at all;
//! the inverted path reads one posting list per query bit and is affine in `m`.
//! Only one side depends on the query, so a single crossover was always a
//! constant for one query width -- it moved **thirtyfold** across the widths
//! measured. `crossover_for` is three coarse regimes on `m / dims`, deliberately
//! not a curve: a power-law fit had a 238% worst residual because the crossover
//! saturates below about 0.08 and a power law cannot.
//!
//! The forward path groups admitted rows by their forward block and visits its
//! words once. Bitmap-backed stores lend the words, so a sparse query touches
//! the rows it needs rather than copying an 8 KiB mask for each 32-byte row.
//! Other stores keep the full-mask fallback. No code layout or score changes.
//! For small `k`, the forward scorer retains only the best hits in a reused
//! max-heap while it walks each block. Materializing all 65 536 hits and then
//! selecting ten was the dominant cost on a measured dense scan. Large `k`
//! keeps the one-shot selection path, which won at the measured large boundary.
//!
//! LIVE and filter-term reads also hold per-worker cursors. Reopening a stream
//! for each posting block rebuilds its whole-key plan each time, making query
//! admission quadratic in the number of blocks even when scoring is linear.
//! Every term occurrence gets its own lane, including repeated terms; retries
//! rebuild all admission lanes against the fresh snapshot along with scoring.
//! Scoring cursors are lazy in both directions: an all-forward scan never
//! opens posting lanes, and an all-inverted scan never opens forward codes.
//! The default inverted kernel visits all posting lanes of a block together
//! and fills bit-sliced planes in word tiles. Its copying fallback leaves
//! stores without borrowed lanes correct; forced plane-wide CSA and ripple
//! remain independent oracles. A failed all-lane visit never exposes a partial
//! score, and retry clears the accumulator before reading the fresh snapshot.
//! Hamming keeps the already-read complement-weight planes for one block so
//! selected hits need no per-document point lookups. Linear-score ties retain
//! only the lowest IDs that can enter that block's top-k.
//! `count()` uses the same admission cursor, and `explain()` holds its LIVE
//! cursor while it walks blocks; neither rebuilds a whole-key plan per block.
//! Auto's executed path depends on each block's admitted count, so `explain()`
//! reports a whole-query placeholder rather than claiming to predict those
//! per-block choices.
//!
//! # Exactness
//!
//! Both paths score every candidate they consider. No bound is consulted and
//! nothing is pruned, so M1's top-k is exact by construction rather than by
//! argument. That is the point of doing it first: M5's pruning is then a change
//! that must not alter any answer, checked against a path that never pruned.
//!
//! Opaque integer row scoring reuses forward admission and borrowing without
//! moving codec math into core. Its serial top-k is block-local until every
//! forward chunk succeeds, so an eviction cannot leak partial scores into a
//! retry. Large unfiltered scans divide disjoint ID ranges across workers that
//! share one immutable snapshot; an eviction discards the whole parallel pass.
//! Measured small and filtered scans remain serial.

use std::collections::BinaryHeap;

use crate::Result;
use crate::code::{CodeRef, DocId};
use crate::index::Index;
use crate::score::{Hit, Metric};
use crate::slice::{self, BlockMask, Slice, zero_mask};
use crate::store::{SetSnapshot, SetStore, Version};

/// Exact signed-integer scoring over one stored forward row.
///
/// Core owns filtering, borrowed row traversal, retry isolation and top-k. The
/// scorer owns only the interpretation of a row. Keeping that boundary opaque
/// lets codecs define their own exact integer math without teaching the index
/// about an embedding format.
pub trait RowScorer: Sync {
    /// Return the row's complete ranking score. Larger is better.
    fn score(&self, row: &[u64]) -> i64;
}

impl<F> RowScorer for F
where
    F: Fn(&[u64]) -> i64 + Sync,
{
    fn score(&self, row: &[u64]) -> i64 {
        self(row)
    }
}

/// Which documents a search may return.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub enum Filter {
    /// Every live document.
    #[default]
    All,
    /// Nothing.
    None,
    /// Documents carrying an attribute.
    Term(u32),
    /// An explicit set.
    Ids(Vec<DocId>),
    /// Documents in `[lo, hi)`.
    IdRange(u64, u64),
    /// Every clause.
    And(Vec<Filter>),
    /// Any clause.
    Or(Vec<Filter>),
    /// The complement, within the live set.
    Not(Box<Filter>),
}

impl Filter {
    /// Reject every term before constructing keys or opening admission lanes.
    fn validate_terms(&self) -> Result<()> {
        match self {
            Self::Term(term) => crate::KeySpace::check_term(*term),
            Self::And(children) | Self::Or(children) => {
                for child in children {
                    child.validate_terms()?;
                }
                Ok(())
            }
            Self::Not(child) => child.validate_terms(),
            _ => Ok(()),
        }
    }

    /// Evaluate into one block's mask, intersected with `live`.
    ///
    /// # Per block, not per corpus
    ///
    /// This replaced a `resolve` that built the live set and the admitted set as
    /// `BTreeSet`s and handed them to the scan. That was `O(N)` allocations on
    /// **every query** -- 4.4 million at 196 000 documents -- which dwarfed
    /// everything the kernels do and made the inverted path economical only at
    /// small sizes. Evaluating a block at a time costs one reused mask per
    /// operand and nothing per document.
    ///
    /// `Not` is complemented **within the live set** rather than over the whole
    /// ordinal universe: an unbounded complement spans 2^64 and is meaningful
    /// for a count, not for a candidate list.
    fn eval_block(
        &self,
        block: u64,
        live: &BlockMask,
        out: &mut BlockMask,
        read_term: &mut impl FnMut(u32, &mut BlockMask) -> Result<()>,
    ) -> Result<()> {
        match self {
            Self::All => *out = *live,
            Self::None => out.fill(0),
            Self::Term(t) => {
                read_term(*t, out)?;
                and_into(out, live);
            }
            Self::Ids(ids) => {
                out.fill(0);
                let base = block * crate::keyspace::BLOCK_ORDINALS;
                let end = base + crate::keyspace::BLOCK_ORDINALS;
                for d in ids {
                    let o = d.get();
                    if (base..end).contains(&o) {
                        let b = (o - base) as usize;
                        out[b >> 6] |= 1u64 << (b & 63);
                    }
                }
                and_into(out, live);
            }
            Self::IdRange(lo, hi) => {
                let base = block * crate::keyspace::BLOCK_ORDINALS;
                for (word, (slot, &live_word)) in out.iter_mut().zip(live).enumerate() {
                    let word_base = base + word as u64 * 64;
                    let from = lo.saturating_sub(word_base).min(64) as u32;
                    let to = hi.saturating_sub(word_base).min(64) as u32;
                    let below_to = if to == 64 { u64::MAX } else { (1u64 << to) - 1 };
                    let from_up = u64::MAX.checked_shl(from).unwrap_or(0);
                    *slot = live_word & below_to & from_up;
                }
            }
            Self::And(cs) => {
                *out = *live;
                for c in cs {
                    let mut sub = zero_mask();
                    c.eval_block(block, live, &mut sub, read_term)?;
                    and_into(out, &sub);
                }
            }
            Self::Or(cs) => {
                out.fill(0);
                for c in cs {
                    let mut sub = zero_mask();
                    c.eval_block(block, live, &mut sub, read_term)?;
                    for (o, s) in out.iter_mut().zip(sub.iter()) {
                        *o |= *s;
                    }
                }
            }
            Self::Not(c) => {
                let mut sub = zero_mask();
                c.eval_block(block, live, &mut sub, read_term)?;
                for ((o, l), s) in out.iter_mut().zip(live.iter()).zip(sub.iter()) {
                    *o = *l & !*s;
                }
            }
        }
        Ok(())
    }

    /// Match evaluation order, without deduplicating: repeated terms must not
    /// rewind the same lane within a block and reopen its whole-key plan.
    fn append_term_keys(&self, keys: crate::KeySpace, out: &mut Vec<u64>) {
        match self {
            Self::Term(t) => out.push(keys.attr(*t)),
            Self::And(cs) | Self::Or(cs) => {
                for c in cs {
                    c.append_term_keys(keys, out);
                }
            }
            Self::Not(c) => c.append_term_keys(keys, out),
            _ => {}
        }
    }
}

/// Per-worker admission streams: LIVE, then one lane per filter occurrence.
///
/// In the persisted D=256, every-1024th-ID fixture, reopening LIVE and ATTR
/// streams per block cost 0.37-0.40 ms at 4 194 304 documents but 4.02-4.07 ms
/// at 16 777 216 (five wrapper-timed samples, 2026-09-20). Both keys' plans grow
/// with the corpus. Hold their positions just as the scoring lanes do; a store
/// declining cursors retains the original addressed-read implementation.
struct Admission {
    keys: Vec<u64>,
    lanes: Option<Box<dyn crate::store::Lanes>>,
}

impl Admission {
    fn new(filter: &Filter, keys: crate::KeySpace, snap: &impl SetSnapshot) -> Result<Self> {
        filter.validate_terms()?;
        let mut all = vec![keys.live()];
        filter.append_term_keys(keys, &mut all);
        let lanes = snap.open_lanes(&all)?;
        Ok(Self { keys: all, lanes })
    }

    fn reopen(&mut self, snap: &impl SetSnapshot) -> Result<()> {
        self.lanes = snap.open_lanes(&self.keys)?;
        Ok(())
    }

    fn live(&mut self, snap: &impl SetSnapshot, block: u64, out: &mut BlockMask) -> Result<bool> {
        read_lane(snap, &mut self.lanes, 0, self.keys[0], block, out)
    }

    fn evaluate(
        &mut self,
        filter: &Filter,
        snap: &impl SetSnapshot,
        block: u64,
        live: &BlockMask,
        out: &mut BlockMask,
    ) -> Result<()> {
        let mut lane = 1;
        filter.eval_block(block, live, out, &mut |term, mask| {
            let key = self.keys[lane];
            debug_assert_eq!(key & crate::KeySpace::INDEX_MAX, u64::from(term));
            read_lane(snap, &mut self.lanes, lane, key, block, mask)?;
            lane += 1;
            Ok(())
        })?;
        debug_assert_eq!(lane, self.keys.len());
        Ok(())
    }
}

fn and_into(a: &mut BlockMask, b: &BlockMask) {
    for (x, y) in a.iter_mut().zip(b.iter()) {
        *x &= *y;
    }
}

/// Which scoring path ran.
/// Which scoring path ran.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum Path {
    /// Read only the rows the filter admits.
    Gather,
    /// Walk every live row.
    DenseScan,
    /// Accumulate bit-sliced over the inverted posting lists.
    Inverted,
}

/// Which exact accumulator the inverted path uses.
///
/// The word-tiled kernel is the default. Plane-wide carry-save and ripple
/// remain independently selectable oracles for differential tests.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
#[non_exhaustive]
pub enum Kernel {
    /// Word-tiled Harley-Seal over simultaneous borrowed lanes.
    #[default]
    Tiled,
    /// Plane-wide Harley-Seal carry-save, retained as an oracle.
    CarrySave,
    /// Ripple-carry, the slow and obvious oracle.
    Ripple,
}

/// Force a path, or let the planner choose.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
#[non_exhaustive]
pub enum PathHint {
    /// Let the planner decide.
    #[default]
    Auto,
    /// Force [`Path::Gather`].
    Gather,
    /// Force [`Path::DenseScan`].
    DenseScan,
    /// Force [`Path::Inverted`].
    Inverted,
}

/// What a scan does when its snapshot is evicted underneath it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum Consistency {
    /// One snapshot for the whole scan. An eviction surfaces as
    /// [`Error::SnapshotExpired`] and the caller decides.
    ///
    /// [`Error::SnapshotExpired`]: crate::Error::SnapshotExpired
    Strict,
    /// Take a fresh snapshot and continue from the block that failed.
    ///
    /// # What this trades away, stated because it is not free
    ///
    /// The result then spans versions. A document deleted between them may
    /// still appear, and one inserted may be missed -- **the answer is exact
    /// for no single version**. It remains exact for each block against the
    /// version that block was read at, which is the most a resumed scan can
    /// promise. [`Hits::version_range`] reports the span so a caller can see
    /// when it happened, and `ScanStats::resumes` counts it.
    ResumeOnEviction {
        /// How many fresh snapshots to take before giving up.
        max_retries: u32,
    },
}

impl Default for Consistency {
    fn default() -> Self {
        Self::ResumeOnEviction { max_retries: 3 }
    }
}

/// Pick the forward path when the filter admits at most one document in this
/// many.
///
/// **Measured**, 262 144 documents at D=256 with a 118-bit query, milliseconds:
///
/// ```text
/// admitted   1 in 8   1 in 12   1 in 16   1 in 20   1 in 24   1 in 32   1 in 256
/// inverted     0.89      0.84      0.86      0.87      0.86      0.85      0.83
/// forward      1.13      0.95      0.91      0.88      0.81      0.77      0.65
/// ```
///
/// The crossing is between 1 in 20 and 1 in 24, and **the two paths are within
/// 6% of each other from 1 in 12 to 1 in 24** -- a wide flat region where the
/// choice barely matters and run-to-run variance is the same size as the gap.
/// 24 sits at its far edge. There is no point pretending to a precision three
/// runs cannot support, and nothing in this constant's history suggests the
/// exact value was ever what mattered.
///
/// The shape is structural. The inverted path reads every query dimension's
/// posting list for any block holding one admitted document, so its cost is
/// **flat** in selectivity; the forward path reads one row per admitted
/// document, so its cost is **linear** in it. They cross where reading
/// `admitted` rows costs what reading `|Q|` posting lists costs.
///
/// # This constant was wrong once, and the way it was wrong is the warning
///
/// The first measurement put the crossing at **1 in 16**, and that number came
/// from a forward path that was silently reading only its first block -- so it
/// scanned a fraction of the documents and looked far faster than it is. The
/// defect was found by a test asserting the two forced paths agree, not by the
/// benchmark, which had no way to know the work was not being done.
///
/// Re-measure after any change to either read path, and re-measure through a
/// test that checks the answers first.
///
/// # It moved again, for the opposite reason
///
/// It was 256, from the table above when the inverted path cost 2.06 ms at that
/// selectivity. Holding a cursor open across blocks took the inverted path to
/// 0.88 ms and left the forward path where it was, so the crossing moved with
/// it: at 1 in 256 the inverted path now wins by 0.33 ms, and 1 in 384 is the
/// first point where it does not.
///
/// The lesson is that this constant is a **ratio between two paths**, so it
/// moves whenever either one does -- including when the change was an
/// unambiguous improvement to one of them. Speeding up the inverted path
/// without moving this number would have left the planner choosing the forward
/// path across a band where it is now 38% slower.
///
/// The corollary, from the forward path's own 3.6x: a speedup does not move it
/// **unless the speedup lands where the paths cross**. Re-measure either way;
/// do not reason about which.
///
/// # It has now been 16, 256, 384, 24 and 11
///
/// Worth keeping, because earlier large moves followed path defects rather
/// than workload changes. It read 16 from a forward path that was silently
/// scanning one block; 256 once that was fixed; 384 when the inverted path got a
/// held-open cursor; and 24 when the forward path got one too and stopped
/// opening a stream per forward block. The new 11 is a direct re-tune of that
/// last regime on the measured wide-query crossover. The old 16 and 24 agree
/// to within a rounding error for unrelated reasons -- the kind of coincidence
/// that would have made a wrong value look confirmed.
///
/// So a planner constant that drifts a long way is evidence about the paths, not
/// about the workload, and the response is to find out which path changed and
/// why -- not to re-fit it.
///
/// A 2026-10-03 quiet-host sweep on 1,048,576 GloVe-derived D=256 codes,
/// 200 Hamming top-10 queries and the tiled default found Inverted faster at
/// one in 8 ( 2.384 versus Gather's 2.627 ms ) and Gather faster at one in 12
/// ( 2.068 versus Inverted's 2.197 ms ). The old 24 multiplier still chose
/// Inverted at one in 12 through 24. Eleven is the largest integer multiplier
/// that chooses Gather at the measured one-in-12 density: each full Block
/// admits 5,462 rows, so 5,462 * 11 <= 65,536 but 5,462 * 12 > 65,536.
/// At one in 8, 8,192 * 11 > 65,536 and Inverted remains selected. A
/// separate pinned 40-query width sweep had 60-79 set bits of 256; at one in
/// 12, Inverted took 1.197 ms against Gather's 2.233 ms in a quiet cell. Its
/// maximum ratio is 0.309, while the 200-query wide sweep starts at 114/256
/// = 0.445. The 0.40 boundary lies between those measured groups, so the new
/// multiplier cannot send that middle-width group to Gather. Narrower regimes
/// still need a qualified density sweep before their constants move.
const SELECTIVITY_CROSSOVER: u64 = 11;
/// The query's weight **within this index's dimensions**.
///
/// `CodeRef::weight` counts every set bit, and the scan can only match bits
/// below `dims`: `expand` filters them out of the posting-list walk, and the
/// ingest path masks a stored row to `dims` for the same reason, with a comment
/// saying so. `m` was the one place that still counted them.
///
/// The consequence was a wrong score rather than a wrong match. Hamming is
/// `2a - m - w`, so an out-of-range bit shifted every score by a constant and
/// left the order intact; Jaccard is `a/(m + w - a)` and cosine `a/sqrt(m*w)`,
/// where `m` is not a constant offset and **the ranking changes**. Reachable
/// two ways: a sparse query naming a position at or above `dims`, and -- with
/// any `dims` that is not a multiple of 64 -- a dense query with a bit set in
/// the padding between `dims` and `row_bits`, which is an internal detail no
/// caller should have to know about.
///
/// Dropping rather than refusing, because the padding case makes refusal absurd:
/// the caller cannot see the padding. A stored code is still refused when it
/// exceeds the geometry, which is the right asymmetry -- a document outside the
/// index's space is a caller error, a query bit outside it is a term that
/// matches nothing.
fn weight_within(code: CodeRef<'_>, dims: u32) -> u32 {
    match code {
        CodeRef::Dense(w) => w
            .iter()
            .enumerate()
            .map(|(i, x)| {
                let lo = (i as u32).saturating_mul(64);
                if lo >= dims {
                    return 0;
                }
                let keep = dims - lo;
                if keep >= 64 {
                    x.count_ones()
                } else {
                    (x & ((1u64 << keep) - 1)).count_ones()
                }
            })
            .sum(),
        CodeRef::Sparse(p) => {
            u32::try_from(p.iter().filter(|&&b| b < dims).count()).unwrap_or(u32::MAX)
        }
    }
}

/// The crossover for a query of `m` bits over `dims`-bit codes.
///
/// # One constant could not serve, and the grid is why
///
/// The original single crossover was measured at one code width, one corpus
/// size and one query -- 118 bits of 256 -- and applied everywhere. Measuring
/// the grid it was extrapolated across, at D=256 and 262 144 documents, the
/// crossover moves
/// by **thirty-fold** with the query's width:
///
/// ```text
/// |Q|    |Q|/D   inverted still wins up to
/// 118    0.461                     1 in 16
///  59    0.230                    1 in 256
///  40    0.156                    1 in 256
///  30    0.117                    1 in 384
///  20    0.078                    1 in 512
///  15    0.059                    1 in 512
/// ```
///
/// It is **flat in the corpus size** ( the same cells at 262 144 and 1 048 576 )
/// and tracks the *ratio* rather than either width alone ( the same shape at
/// D = 128, 256 and 512 ), which is why this takes `m / dims` and not `m`.
///
/// # Why it moves, which is the part worth keeping
///
/// Gather reads whole rows, so its cost does not depend on the query's width at
/// all: 0.74 ms against 0.70 ms at one in twenty-four, for queries of 118 and 15
/// bits. The inverted path reads one posting list per query bit, so it is affine
/// in `m` -- 0.85 ms at 118, 0.30 ms at 15. **Only one side of the comparison
/// depends on `m`**, so the crossing has to move with it, and a single constant
/// was always going to be a constant for one query width.
///
/// # A step function, and deliberately not a curve
///
/// A power-law fit over these points gives `23.8 * (D/|Q|)^1.06` with a **238%
/// worst residual** -- the crossover saturates below `|Q|/D` of about 0.08 and a
/// power law cannot. This project has already retired one model that fitted its
/// own numbers and nothing else, so what ships is the measured table as three
/// coarse regimes rather than a curve implying a precision the data has not got.
/// The breakpoints are between measured rows, not on them.
///
/// On that historical grid, at `|Q|/D` near 0.12 the table said 1 in 384
/// while the coarse rule returned 256, measuring 1.3x worse than the better
/// path there. The earlier single constant lost up to **2.4x**. Those are
/// dated measurements, not bounds for the current tiled/heap kernels.
///
/// These boundaries predate borrowed forward reads and admission cursors. A
/// 2026-09-20 check after both changes,
/// at D=256, 2 097 152 documents, Hamming k=10, query widths 118/59/15 and
/// strides 16/256/1024 kept the better path in eight of nine cells. At width
/// 15, stride 256, Gather took 2.54-2.55 ms versus Inverted's 2.75-2.78 ms
/// (five runs). The 2026-10-03 sweep moved only the newly measured wide band;
/// middle and thin thresholds stay put until a qualified density/width grid
/// locates their new boundaries.
#[inline]
fn crossover_for(m: u32, dims: u32) -> u64 {
    // Guard the degenerate query: a zero-width code admits nothing and the
    // choice cannot matter, but the division would.
    let ratio = f64::from(m) / f64::from(dims.max(1));
    if ratio >= 0.40 {
        SELECTIVITY_CROSSOVER
    } else if ratio >= 0.10 {
        256
    } else {
        512
    }
}

/// What `scan` already established about one block, handed to whichever path it
/// chose.
///
/// Grouped rather than passed loose: the two routines need the same three
/// facts, and deriving them again inside each would duplicate the work **and**
/// risk disagreeing with the decision that selected the path.
struct BlockCtx<'a> {
    /// The best `k`-th score found in **earlier blocks**, when `k` have been
    /// found at all.
    ///
    /// A valid lower bound on the true `k`-th best score, exactly as a block's
    /// own top-`k` is: any `k` documents' `k`-th best is at most the true one.
    /// The two are different quantities, and the maximum of two valid lower
    /// bounds is a valid lower bound -- so a block refines against whichever is
    /// tighter.
    ///
    /// This is what "iterating the refinement" should have meant. Iterating
    /// *within* a block is a fixed point and is proved so in this module's
    /// tests; the threshold that actually improves comes from documents found
    /// somewhere else.
    floor: Option<crate::Score>,
    block: u64,
    live: &'a BlockMask,
    cand: &'a BlockMask,
}

// On the persisted 1,048,576-row D=256 fixture, the exact per-block heap
// ranked Hamming top-k in 7.7-8.6 ms at k=1,10,32,128, versus 12.9-14.8 ms
// for materialize-then-select. At k=1024 they were both about 14 ms.
// Keep the older selection for larger k until its crossover is measured.
const FORWARD_HEAP_MAX_K: usize = 128;

/// A full hit ordered by the same total ranking key as the final output.
/// The max-heap's root is the worst retained hit.
#[derive(Clone, Copy, PartialEq, Eq)]
struct RankedHit(Hit);

impl Ord for RankedHit {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.rank_key().cmp(&other.0.rank_key())
    }
}

impl PartialOrd for RankedHit {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// Buffers a scan reuses across every block.
///
/// Hoisted out of the per-block routines rather than declared inside them: an
/// earlier refactor left them local, which allocated an accumulator, a slice and
/// three vectors **per block**. The answers were identical and the allocation
/// budget caught it at 13 against 36 -- which is the only instrument that could
/// have, and is why that budget exists.
struct ScanBufs {
    csa: crate::slice::Csa,
    acc: Slice,
    scratch: BlockMask,
    /// Reused by the all-lane visit when a store offers only copied masks.
    lane_scratch: Vec<BlockMask>,
    /// Hamming's complement-weight planes, copied once per block so selected
    /// hits can read their weights without nine point lookups apiece.
    z_masks: Vec<BlockMask>,
    offs: Vec<u32>,
    /// Reused across forward blocks, so top-k does not allocate per block.
    forward_heap: BinaryHeap<RankedHit>,
    dims: Vec<u32>,
    /// A cursor over the query's posting lists and the weight planes, held open
    /// across blocks. `None` before the first inverted block, or when the
    /// store declines cursors and reads go back through `load_block`.
    ///
    /// Lane `i < dims.len()` is `dims[i]`'s posting list; the `z_planes` lanes
    /// after that are the weight planes in order. The scan reads them in that
    /// order and a cursor is free to assume it.
    lanes: Option<Box<dyn crate::store::Lanes>>,
    /// Distinguish unopened from a store declining cursors. A declined open
    /// must not be retried on every inverted block.
    lanes_ready: bool,
    /// The forward codes' cursor, opened on first use.
    fwd: FwdCursor,
}

/// The forward-code cursor, opened only when a forward block needs it.
///
/// Opening a cursor costs one index range scan per key, so it is proportional to
/// that key's chunk count. The forward codes are one key holding every
/// document's row -- 8 192 chunks at two million documents, where a posting list
/// has 32. Measured, that single lane costs **0.224 ms to open against 0.205 ms
/// for all 118 posting lanes together**, a scan on the inverted path never reads
/// it, and a parallel scan would pay it once per worker.
///
/// Three states rather than an `Option`, because "not opened yet" and "this
/// store offers no cursor, keep using `load_block`" are different: collapsing
/// them would re-attempt the expensive open on every forward block for any store
/// that declines.
enum FwdCursor {
    /// Not needed yet. Becomes one of the others at the first forward block.
    Unopened,
    /// Opened, and the store declined to provide one.
    Absent,
    Open(Box<dyn crate::store::Lanes>),
}

impl FwdCursor {
    fn with_block(
        &mut self,
        snap: &impl SetSnapshot,
        key: u64,
        block: u64,
        visit: &mut dyn FnMut(&BlockMask),
    ) -> Result<()> {
        if matches!(self, Self::Unopened) {
            *self = match snap.open_lanes(&[key])? {
                Some(lanes) => Self::Open(lanes),
                None => Self::Absent,
            };
        }
        match self {
            Self::Open(lanes) => lanes.with_block(0, block, visit),
            _ => {
                let mut mask = zero_mask();
                snap.load_block(key, block, &mut mask)?;
                visit(&mask);
                Ok(())
            }
        }
    }

    fn reset(&mut self) {
        *self = Self::Unopened;
    }
}

impl ScanBufs {
    fn new(levels: usize, dims: Vec<u32>) -> Self {
        Self {
            csa: crate::slice::Csa::new(levels),
            acc: Slice::new(levels),
            scratch: zero_mask(),
            lane_scratch: Vec::new(),
            z_masks: Vec::new(),
            offs: Vec::new(),
            forward_heap: BinaryHeap::new(),
            dims,
            lanes: None,
            lanes_ready: false,
            fwd: FwdCursor::Unopened,
        }
    }

    /// The keys this buffer's lanes address, in lane order.
    fn lane_keys(&self, keys: &crate::KeySpace, z_planes: u32) -> Vec<u64> {
        self.dims
            .iter()
            .map(|&d| keys.dim(d))
            .chain((0..z_planes).map(|j| keys.zplane(j)))
            .collect()
    }

    fn with_forward(
        &mut self,
        snap: &impl SetSnapshot,
        key: u64,
        block: u64,
        visit: &mut dyn FnMut(&BlockMask),
    ) -> Result<()> {
        self.fwd.with_block(snap, key, block, visit)
    }

    /// Open inverted lanes only when an admitted block chooses that path.
    ///
    /// Eager setup cost 1.01-1.02 ms at 16 777 216 documents even for forced
    /// Gather (D=256, Hamming k=10, every 1024th ID admitted; five wrapper-timed
    /// runs, 2026-09-20). Auto can switch paths between blocks, so checking only
    /// the query hint is insufficient. Record a declined open as well.
    fn open_lanes(
        &mut self,
        snap: &impl SetSnapshot,
        keys: &crate::KeySpace,
        z: u32,
    ) -> Result<()> {
        if !self.lanes_ready {
            let lane_keys = self.lane_keys(keys, z);
            self.lanes = snap.open_lanes(&lane_keys)?;
            self.lanes_ready = true;
        }
        Ok(())
    }

    /// Both cursor families belong to the old snapshot. Discard them together;
    /// each is reopened against the fresh snapshot only if its path runs again.
    fn reset_cursors(&mut self) {
        self.lanes = None;
        self.lanes_ready = false;
        self.fwd.reset();
    }
}

/// Read one lane, through the cursor if there is one.
///
/// `key` is lane `lane`'s key and the two must agree; it is passed so the
/// fallback has something to address. Keeping both is what lets a store opt out
/// of cursors entirely without the scan growing a second code path.
#[inline]
fn read_lane(
    snap: &impl SetSnapshot,
    lanes: &mut Option<Box<dyn crate::store::Lanes>>,
    lane: usize,
    key: u64,
    block: u64,
    out: &mut BlockMask,
) -> Result<bool> {
    match lanes {
        Some(l) => l.read(lane, block, out),
        None => snap.load_block(key, block, out),
    }
}

/// What a scan actually did.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct ScanStats {
    /// Blocks that held at least one live document.
    pub blocks_visited: u64,
    /// Blocks skipped because the filter admitted nothing in them.
    pub blocks_skipped: u64,
    /// How many times the snapshot was evicted and the scan resumed.
    pub resumes: u32,
}

/// What a search returns.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hits {
    /// Descending by score, then ascending by id.
    pub hits: Vec<Hit>,
    /// Which path produced them. Forcing both and comparing is a test, not a
    /// diagnostic: they must agree exactly.
    pub path: Path,
    /// How many documents were scored.
    pub scored: u64,
    /// The oldest and newest store versions the result was read from. Equal
    /// unless the scan resumed after an eviction.
    pub version_range: (Version, Version),
    /// What the scan did.
    pub stats: ScanStats,
}

/// One exact result from an opaque integer row scorer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RowHit {
    /// Document ordinal.
    pub id: DocId,
    /// Complete signed-integer score supplied by [`RowScorer`].
    pub score: i64,
}

impl RowHit {
    fn rank_key(&self) -> (std::cmp::Reverse<i64>, DocId) {
        (std::cmp::Reverse(self.score), self.id)
    }
}

/// Results from exact filtered forward-row scoring.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RowHits {
    /// Descending by score, then ascending by id.
    pub hits: Vec<RowHit>,
    /// How many documents were scored.
    pub scored: u64,
    /// The oldest and newest store versions read.
    pub version_range: (Version, Version),
    /// What the scan did.
    pub stats: ScanStats,
}

/// What a search would do, without doing it.
///
/// Reports decisions and cheap facts only. It deliberately does **not** predict
/// cost: there is no measured cost model here yet, and a plausible number
/// carrying no measurement is worse than an absent one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    /// The path that would run.
    pub path: Path,
    /// Why, in one line.
    pub path_reason: &'static str,
    /// The accumulator, if the inverted path runs.
    pub kernel: Kernel,
    /// The similarity.
    pub metric: Metric,
    /// How many results are wanted.
    pub k: usize,
    /// Set bits in the query.
    pub query_bits: u32,
    /// Bits per code.
    pub dims: u32,
    /// Planes the accumulator needs.
    pub accumulator_levels: usize,
    /// Blocks that could hold a live document.
    pub blocks: u64,
    /// How many of those carry statistics, and can therefore use the tightened
    /// bound. A live code write or delete removes its block's entry until
    /// `refresh_stats` runs again.
    pub blocks_with_stats: u64,
    /// The mean of `w_max - w_min` over blocks carrying statistics, or `None`
    /// when none do.
    ///
    /// **An observation, not a prediction.** It is reported because the thing it
    /// observes is worth 40x on the ratio metrics and is invisible from
    /// behaviour: a Jaccard bound can only skip a document whose weight it can
    /// rule out, so it prunes in proportion to how narrow a block's weight range
    /// is -- and that range is narrow only if the caller assigned ids in weight
    /// order. Measured at 524 288 documents and D=256, a top-10 Jaccard query
    /// scores 126 160 documents on arbitrarily ordered ids and 3 136 on
    /// weight-ordered ones.
    ///
    /// Compare it to `dims`. A spread near `dims / 2` means ids are in no
    /// particular weight order; a small one means they are. haiiie does not act
    /// on this and does not reorder anything -- ids are the caller's, and
    /// reassigning them is the recycling the design forbids.
    pub block_weight_spread: Option<u32>,
    /// Whether the ranking key is exact, or a bound plus a refinement pass.
    pub exactness: &'static str,
}

impl std::fmt::Display for Plan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "path      {:?}  ({})", self.path, self.path_reason)?;
        if self.path == Path::Inverted {
            writeln!(f, "kernel    {:?}", self.kernel)?;
            writeln!(
                f,
                "accum     {} planes over 65536-ordinal blocks",
                self.accumulator_levels
            )?;
        }
        writeln!(
            f,
            "metric    {:?}  k={}  ({})",
            self.metric, self.k, self.exactness
        )?;
        writeln!(f, "query     {} of {} bits set", self.query_bits, self.dims)?;
        write!(
            f,
            "blocks    {} live, {} with statistics{}",
            self.blocks,
            self.blocks_with_stats,
            if self.blocks_with_stats < self.blocks {
                "  (run refresh_stats to tighten the rest)"
            } else {
                ""
            }
        )?;
        // Only for the ratio metrics: it is a fact about the index either way,
        // but it is only a fact the reader can act on when it is costing them.
        if let Some(spread) = self.block_weight_spread
            && matches!(self.metric, Metric::Jaccard | Metric::Cosine)
        {
            write!(
                f,
                "\nweights   blocks span {spread} of {} bits on average{}",
                self.dims,
                if spread * 4 > self.dims {
                    "  (ids are not in weight order; this metric prunes far less)"
                } else {
                    ""
                }
            )?;
        }
        Ok(())
    }
}

/// A search under construction.
#[derive(Debug)]
pub struct Search<'i, 'q, S: SetStore> {
    index: &'i Index<S>,
    code: Option<CodeRef<'q>>,
    metric: Metric,
    k: usize,
    filter: Filter,
    hint: PathHint,
    kernel: Kernel,
    consistency: Consistency,
    threads: usize,
}

/// An exact filtered forward-row search under construction.
#[derive(Debug)]
pub struct RowSearch<'i, 's, S: SetStore, R: RowScorer + ?Sized> {
    index: &'i Index<S>,
    scorer: &'s R,
    k: usize,
    filter: Filter,
    consistency: Consistency,
    threads: usize,
}

impl<S: SetStore> Index<S> {
    /// Begin a search.
    #[must_use]
    pub fn search(&self) -> Search<'_, '_, S> {
        Search {
            index: self,
            code: None,
            metric: Metric::default(),
            k: 10,
            filter: Filter::All,
            hint: PathHint::default(),
            kernel: Kernel::default(),
            consistency: Consistency::default(),
            threads: 1,
        }
    }

    /// Begin exact signed-integer scoring over stored forward rows.
    ///
    /// This always uses the filter-aware borrowed-row path. A block interrupted
    /// by snapshot eviction is discarded in full before retry, including scores
    /// computed from earlier forward chunks in that block.
    #[must_use]
    pub fn row_search<'i, 's, R: RowScorer + ?Sized>(
        &'i self,
        scorer: &'s R,
    ) -> RowSearch<'i, 's, S, R> {
        RowSearch {
            index: self,
            scorer,
            k: 10,
            filter: Filter::All,
            consistency: Consistency::default(),
            threads: 1,
        }
    }
}

// Reopened 512-bit residual indexes measured at 1,024 through 44,356 live rows,
// 128 queries per sample and five interleaved samples per arm. At 16,384 rows
// the serial 0.751-0.944 ms and eight-worker 0.554-0.827 ms ranges overlap. At
// 32,768 they separate at 1.503-1.691 ms versus 0.873-1.282 ms. Use the first
// production-path size whose complete ranges separate, rather than the lower
// crossing suggested by the shorter prototype measurement.
const ROW_PARALLEL_MIN_LIVE: u64 = 32_768;
// Eight workers produced the best repeated range at 32,768 and 44,356 rows.
// Requests above eight use eight; the storage layer's locks make more workers
// overhead rather than more independent reads on this measured machine.
const ROW_PARALLEL_MAX_WORKERS: usize = 8;
type RowRangeResult = (Vec<RowHit>, u64, Vec<(u64, bool)>);

impl<S: SetStore, R: RowScorer + ?Sized> RowSearch<'_, '_, S, R> {
    /// How many results to return.
    #[must_use]
    pub fn k(mut self, k: usize) -> Self {
        self.k = k;
        self
    }

    /// Restrict the candidates.
    #[must_use]
    pub fn filter(mut self, filter: Filter) -> Self {
        self.filter = filter;
        self
    }

    /// What to do if the snapshot is evicted mid-scan.
    #[must_use]
    pub fn consistency(mut self, consistency: Consistency) -> Self {
        self.consistency = consistency;
        self
    }

    /// Allow this many scoring workers for a large unfiltered scan.
    ///
    /// Filtered row scans remain serial: a 694-row scattered filter measured
    /// 0.125-0.252 ms serial and 0.803-0.879 ms at eight workers. Unfiltered
    /// scans engage only at the measured 32,768-live-row crossover, and use at
    /// most the measured eight-worker optimum. One, the default, is serial.
    #[must_use]
    pub fn threads(mut self, n: usize) -> Self {
        self.threads = n.max(1);
        self
    }

    /// Score every admitted row and return its true top-k.
    pub fn execute(self) -> Result<RowHits> {
        self.filter.validate_terms()?;
        let initial = self.index.store().snapshot()?;
        let keys = self.index.keys();
        let last = last_block(&initial, keys)?;
        let live_count = initial.cardinality(keys.live())?;
        let version = initial.version();
        let Some(last) = last else {
            return Ok(RowHits {
                hits: Vec::new(),
                scored: 0,
                version_range: (version, version),
                stats: ScanStats::default(),
            });
        };

        if self.threads > 1
            && live_count >= ROW_PARALLEL_MIN_LIVE
            && matches!(&self.filter, Filter::All)
        {
            // The parallel attempt computes its bound on its own shared
            // snapshot. Its admission count is only a scheduling decision.
            drop(initial);
            return self.execute_parallel();
        }

        let meta = self.index.meta();
        let mut snap = initial;
        let mut admission = Admission::new(&self.filter, keys, &snap)?;
        let (mut vmin, mut vmax) = (snap.version(), snap.version());
        let mut stats = ScanStats::default();
        let mut hits = Vec::new();
        let mut block_top = std::collections::BinaryHeap::new();
        let mut block_hits = Vec::new();
        let mut scored = 0u64;
        let mut live = zero_mask();
        let mut admitted = zero_mask();
        let mut offs = Vec::new();
        let mut fwd = FwdCursor::Unopened;
        let mut block = 0u64;

        while block <= last {
            block_top.clear();
            block_hits.clear();
            let outcome = (|| -> Result<Option<u64>> {
                if !admission.live(&snap, block, &mut live)? {
                    return Ok(None);
                }
                admission.evaluate(&self.filter, &snap, block, &live, &mut admitted)?;
                if crate::slice::popcount(&admitted) == 0 {
                    return Ok(None);
                }

                crate::slice::offsets(&admitted, &mut offs);
                let admitted_count = offs.len() as u64;
                let mut next = 0;
                while next < offs.len() {
                    let id = DocId(block * crate::keyspace::BLOCK_ORDINALS + u64::from(offs[next]));
                    let addr = self.index.row_addr(id);
                    let end_id = (addr.block + 1) * u64::from(meta.rows_per_block);
                    let start = next;
                    while next < offs.len()
                        && block * crate::keyspace::BLOCK_ORDINALS + u64::from(offs[next]) < end_id
                    {
                        next += 1;
                    }
                    fwd.with_block(&snap, keys.forward(), addr.block, &mut |mask| {
                        for &off in &offs[start..next] {
                            let id =
                                DocId(block * crate::keyspace::BLOCK_ORDINALS + u64::from(off));
                            let row_addr = self.index.row_addr(id);
                            let lo =
                                (row_addr.base % crate::keyspace::BLOCK_ORDINALS) as usize / 64;
                            let row = &mask[lo..lo + (meta.row_bits / 64) as usize];
                            let score = self.scorer.score(row);
                            let rank = (std::cmp::Reverse(score), id);
                            if block_top.len() < self.k {
                                block_top.push(rank);
                            } else if block_top.peek().is_some_and(|worst| rank < *worst) {
                                block_top.pop();
                                block_top.push(rank);
                            }
                        }
                    })?;
                }
                block_hits.extend(
                    block_top
                        .drain()
                        .map(|(score, id)| RowHit { id, score: score.0 }),
                );
                Ok(Some(admitted_count))
            })();

            match outcome {
                Ok(visited) => {
                    match visited {
                        Some(n) => {
                            stats.blocks_visited += 1;
                            scored += n;
                        }
                        None => stats.blocks_skipped += 1,
                    }
                    hits.append(&mut block_hits);
                    if hits.len() > self.k {
                        hits.sort_by_key(RowHit::rank_key);
                        hits.truncate(self.k);
                    }
                    block += 1;
                }
                Err(error) if error.is_snapshot_expired() => {
                    let retries = match self.consistency {
                        Consistency::ResumeOnEviction { max_retries } => max_retries,
                        Consistency::Strict => 0,
                    };
                    if stats.resumes >= retries {
                        return Err(error);
                    }
                    snap = self.index.store().snapshot()?;
                    admission.reopen(&snap)?;
                    fwd.reset();
                    vmin = vmin.min(snap.version());
                    vmax = vmax.max(snap.version());
                    stats.resumes += 1;
                }
                Err(error) => return Err(error),
            }
        }

        hits.sort_by_key(RowHit::rank_key);
        hits.truncate(self.k);
        Ok(RowHits {
            hits,
            scored,
            version_range: (vmin, vmax),
            stats,
        })
    }

    /// Run disjoint ID ranges against one immutable snapshot and merge their
    /// exact local top-k lists. An eviction discards the whole attempt: keeping
    /// successful shards and reopening only a failed one would mix versions.
    fn execute_parallel(self) -> Result<RowHits> {
        let retries = match self.consistency {
            Consistency::ResumeOnEviction { max_retries } => max_retries,
            Consistency::Strict => 0,
        };
        let mut resumes = 0;
        let mut versions: Option<(Version, Version)> = None;

        loop {
            let snap = self.index.store().snapshot()?;
            let version = snap.version();
            versions = Some(match versions {
                Some((lo, hi)) => (lo.min(version), hi.max(version)),
                None => (version, version),
            });
            let attempt = (|| -> Result<RowHits> {
                let Some(last_doc) = snap.max(self.index.keys().live())? else {
                    return Ok(RowHits {
                        hits: Vec::new(),
                        scored: 0,
                        version_range: (version, version),
                        stats: ScanStats::default(),
                    });
                };
                let ordinal_span =
                    usize::try_from(last_doc.saturating_add(1)).unwrap_or(usize::MAX);
                let workers = self.threads.min(ROW_PARALLEL_MAX_WORKERS).min(ordinal_span);
                self.parallel_attempt(&snap, last_doc, workers)
            })();

            match attempt {
                Ok(mut result) => {
                    result.version_range = versions.expect("the attempt recorded a version");
                    result.stats.resumes = resumes;
                    return Ok(result);
                }
                Err(error) if error.is_snapshot_expired() && resumes < retries => {
                    resumes += 1;
                }
                Err(error) => return Err(error),
            }
        }
    }

    fn parallel_attempt(&self, snap: &S::Snap, last_doc: u64, workers: usize) -> Result<RowHits> {
        let end = last_doc + 1;
        let width = end.div_ceil(workers as u64);
        let results: Vec<Result<RowRangeResult>> = std::thread::scope(|scope| {
            let handles = (0..workers)
                .filter_map(|worker| {
                    let lo = (worker as u64) * width;
                    let hi = ((worker as u64 + 1) * width).min(end);
                    (lo < hi).then(|| scope.spawn(move || self.score_range(snap, lo, hi)))
                })
                .collect::<Vec<_>>();
            handles
                .into_iter()
                .map(|handle| handle.join().expect("a row scan worker panicked"))
                .collect()
        });

        let mut hits = Vec::with_capacity(workers.saturating_mul(self.k));
        let mut scored = 0u64;
        let mut blocks = std::collections::BTreeMap::new();
        for result in results {
            let (mut local_hits, local_scored, local_blocks) = result?;
            hits.append(&mut local_hits);
            scored += local_scored;
            for (block, visited) in local_blocks {
                blocks
                    .entry(block)
                    .and_modify(|old| *old |= visited)
                    .or_insert(visited);
            }
        }
        hits.sort_by_key(RowHit::rank_key);
        hits.truncate(self.k);
        let stats = ScanStats {
            blocks_visited: blocks.values().filter(|&&visited| visited).count() as u64,
            blocks_skipped: blocks.values().filter(|&&visited| !visited).count() as u64,
            resumes: 0,
        };
        let version = snap.version();
        Ok(RowHits {
            hits,
            scored,
            version_range: (version, version),
            stats,
        })
    }

    fn score_range(&self, snap: &S::Snap, lo: u64, hi: u64) -> Result<RowRangeResult> {
        let meta = self.index.meta();
        let keys = self.index.keys();
        let filter = Filter::IdRange(lo, hi);
        let mut admission = Admission::new(&filter, keys, snap)?;
        let mut live = zero_mask();
        let mut admitted = zero_mask();
        let mut offs = Vec::new();
        let mut fwd = FwdCursor::Unopened;
        let mut top = std::collections::BinaryHeap::new();
        let mut scored = 0u64;
        let mut blocks = Vec::new();
        let mut block = crate::keyspace::block_of(lo);
        let last = crate::keyspace::block_of(hi - 1);

        while block <= last {
            if !admission.live(snap, block, &mut live)? {
                blocks.push((block, false));
                block += 1;
                continue;
            }
            admission.evaluate(&filter, snap, block, &live, &mut admitted)?;
            if crate::slice::popcount(&admitted) == 0 {
                blocks.push((block, false));
                block += 1;
                continue;
            }

            blocks.push((block, true));
            crate::slice::offsets(&admitted, &mut offs);
            scored += offs.len() as u64;
            let mut next = 0;
            while next < offs.len() {
                let id = DocId(block * crate::keyspace::BLOCK_ORDINALS + u64::from(offs[next]));
                let addr = self.index.row_addr(id);
                let end_id = (addr.block + 1) * u64::from(meta.rows_per_block);
                let start = next;
                while next < offs.len()
                    && block * crate::keyspace::BLOCK_ORDINALS + u64::from(offs[next]) < end_id
                {
                    next += 1;
                }
                fwd.with_block(snap, keys.forward(), addr.block, &mut |mask| {
                    for &off in &offs[start..next] {
                        let id = DocId(block * crate::keyspace::BLOCK_ORDINALS + u64::from(off));
                        let row_addr = self.index.row_addr(id);
                        let start = (row_addr.base % crate::keyspace::BLOCK_ORDINALS) as usize / 64;
                        let row = &mask[start..start + (meta.row_bits / 64) as usize];
                        let score = self.scorer.score(row);
                        let rank = (std::cmp::Reverse(score), id);
                        if top.len() < self.k {
                            top.push(rank);
                        } else if top.peek().is_some_and(|worst| rank < *worst) {
                            top.pop();
                            top.push(rank);
                        }
                    }
                })?;
            }
            block += 1;
        }

        let mut hits = top
            .drain()
            .map(|(score, id)| RowHit { id, score: score.0 })
            .collect::<Vec<_>>();
        hits.sort_by_key(RowHit::rank_key);
        Ok((hits, scored, blocks))
    }
}

impl<'i, 'q, S: SetStore> Search<'i, 'q, S> {
    /// The query code.
    #[must_use]
    pub fn code(mut self, q: CodeRef<'q>) -> Self {
        self.code = Some(q);
        self
    }
    /// The similarity to rank by.
    #[must_use]
    pub fn metric(mut self, m: Metric) -> Self {
        self.metric = m;
        self
    }
    /// How many results.
    #[must_use]
    pub fn k(mut self, k: usize) -> Self {
        self.k = k;
        self
    }
    /// Restrict the candidates.
    #[must_use]
    pub fn filter(mut self, f: Filter) -> Self {
        self.filter = f;
        self
    }
    /// Force a path. Used by the differential tests; `Auto` otherwise.
    #[must_use]
    pub fn path(mut self, h: PathHint) -> Self {
        self.hint = h;
        self
    }

    /// Force an accumulator. Used by the differential tests only.
    #[must_use]
    pub fn kernel(mut self, k: Kernel) -> Self {
        self.kernel = k;
        self
    }

    /// What to do if the snapshot is evicted mid-scan.
    #[must_use]
    pub fn consistency(mut self, c: Consistency) -> Self {
        self.consistency = c;
        self
    }

    /// Scan with this many threads. One, the default, is serial.
    ///
    /// # The answer does not depend on this
    ///
    /// Blocks are independent, and the top-k of a union is the top-k of the
    /// parts' top-k's -- the same argument the serial scan already uses to merge
    /// blocks, which does not care whether the parts were produced in sequence
    /// or at once. Results are byte-identical at any thread count, and a test
    /// asserts it rather than the doc comment claiming it.
    ///
    /// # Why the default is serial rather than the core count
    ///
    /// Because changing what a program does when nobody asked is how a library
    /// surprises its callers, and because an embedded caller is often already
    /// parallel at a level this knows nothing about -- spending its cores on one
    /// query would be the wrong trade made on its behalf. The server opts in.
    #[must_use]
    pub fn threads(mut self, n: usize) -> Self {
        self.threads = n.max(1);
        self
    }

    /// How many documents the filter admits, without scoring any of them.
    pub fn count(self) -> Result<u64> {
        self.filter.validate_terms()?;
        let snap = self.index.store().snapshot()?;
        let keys = self.index.keys();
        let mut live = zero_mask();
        let mut adm = zero_mask();
        let mut n = 0u64;
        let Some(last) = last_block(&snap, keys)? else {
            return Ok(0);
        };
        let mut admission = Admission::new(&self.filter, keys, &snap)?;
        for block in 0..=last {
            if !admission.live(&snap, block, &mut live)? {
                continue;
            }
            admission.evaluate(&self.filter, &snap, block, &live, &mut adm)?;
            n += u64::from(crate::slice::popcount(&adm));
        }
        Ok(n)
    }

    /// Describe what [`Search::execute`] would do, without scoring anything.
    pub fn explain(self) -> Result<Plan> {
        self.filter.validate_terms()?;
        if self.index.meta().is_residual() {
            return Err(crate::Error::BinarySearchOnResidualIndex);
        }
        let meta = self.index.meta();
        let keys = self.index.keys();
        let query = self.code.unwrap_or(CodeRef::Dense(&[]));
        let m = weight_within(query, self.index.meta().dims);
        let snap = self.index.store().snapshot()?;

        let mut blocks = 0u64;
        let mut with_stats = 0u64;
        let mut spread_total = 0u64;
        let mut live = zero_mask();
        if let Some(last) = last_block(&snap, keys)? {
            let mut live_lane = snap.open_lanes(&[keys.live()])?;
            for block in 0..=last {
                if read_lane(&snap, &mut live_lane, 0, keys.live(), block, &mut live)? {
                    blocks += 1;
                    if let Some(st) =
                        crate::BlockStats::from_ordinals(&snap.load(keys.stat(block))?)
                    {
                        with_stats += 1;
                        spread_total += u64::from(st.w_max - st.w_min);
                    }
                }
            }
        }

        let (path, path_reason) = self.plan_path();
        let max = match self.metric {
            Metric::Hamming => 2 * m + meta.dims,
            _ => m,
        };
        Ok(Plan {
            path,
            path_reason,
            kernel: self.kernel,
            metric: self.metric,
            k: self.k,
            query_bits: m,
            dims: meta.dims,
            accumulator_levels: crate::slice::levels_for(max.max(1)),
            blocks,
            blocks_with_stats: with_stats,
            // Mean rather than max: one outlying block says little, and the
            // question a reader has is whether their ids are in weight order at
            // all, which is a property of the whole index.
            block_weight_spread: (with_stats > 0)
                .then(|| u32::try_from(spread_total / with_stats).unwrap_or(u32::MAX)),
            exactness: match self.metric {
                Metric::Dot | Metric::Hamming => "exact ranking key, descent is the answer",
                _ => "ratio: monotone bound plus a refinement pass",
            },
        })
    }

    fn plan_path(&self) -> (Path, &'static str) {
        match self.hint {
            PathHint::Gather => (Path::Gather, "forced"),
            PathHint::DenseScan => (Path::DenseScan, "forced"),
            PathHint::Inverted => (Path::Inverted, "forced"),
            PathHint::Auto => (
                Path::Inverted,
                "Auto chooses per block after filter admission. Inverted is a \
                 whole-query placeholder, not a prediction of the path used \
                 by execute; explain does not run the per-block planner",
            ),
        }
    }

    /// Run the search.
    pub fn execute(self) -> Result<Hits> {
        if self.index.meta().is_residual() {
            return Err(crate::Error::BinarySearchOnResidualIndex);
        }
        self.filter.validate_terms()?;
        let query = self.code.unwrap_or(CodeRef::Dense(&[]));
        let snap = self.index.store().snapshot()?;
        let keys = self.index.keys();
        let last = last_block(&snap, keys)?;
        let version = snap.version();

        let (path, _) = self.plan_path();

        // Keep the snapshot that supplied `last`: a second open could land on
        // a newer version with documents beyond that bound, then report a
        // complete answer for a range it never scanned.
        // No live document anywhere: no block to visit, and in particular not
        // block zero. Returning here rather than scanning `0 ..= 0` is what
        // keeps `blocks_skipped` honest on an empty index.
        let Some(last) = last else {
            return Ok(Hits {
                hits: Vec::new(),
                path,
                scored: 0,
                version_range: (version, version),
                stats: ScanStats::default(),
            });
        };
        // Thread startup is a fixed cost against a per-block gain, so a scan
        // with few blocks cannot win. Four is a guess and is marked as one:
        // the crossing has not been measured, and the cost of guessing low is
        // bounded by the thread spawn.
        if self.threads > 1 && last >= 4 {
            self.scan_parallel(query, last, path, snap)
        } else {
            self.scan(query, last, path, snap)
        }
    }

    /// Walk the blocks, taking a fresh snapshot if one is evicted underneath.
    ///
    /// # A retry must discard the failed block's work, not resume inside it
    ///
    /// The per-block routines write their hits into a scratch vector that is
    /// cleared on entry and merged into the result only on success. Without
    /// that, a block interrupted halfway and retried would contribute some of
    /// its documents twice -- which sorts and truncates into a plausible
    /// answer, so no assertion on the result's shape would catch it.
    fn scan(&self, query: CodeRef<'_>, last: u64, path: Path, mut snap: S::Snap) -> Result<Hits> {
        let meta = self.index.meta();
        let m = weight_within(query, meta.dims);
        let max = match self.metric {
            Metric::Hamming => 2 * m + meta.dims,
            _ => m,
        };
        let mut bufs = ScanBufs::new(
            crate::slice::levels_for(max.max(1)),
            query_dims(query, meta.dims),
        );

        let keys = self.index.keys();
        let mut admission = Admission::new(&self.filter, keys, &snap)?;
        let (mut vmin, mut vmax) = (snap.version(), snap.version());
        let mut stats = ScanStats::default();
        let mut hits: Vec<Hit> = Vec::new();
        // The `k`-th best score found so far, once `k` exist. See `BlockCtx`.
        let mut floor: Option<crate::Score> = None;
        let mut block_hits: Vec<Hit> = Vec::new();
        let mut scored = 0u64;
        let mut block = 0u64;
        let (mut live, mut cand) = (zero_mask(), zero_mask());
        let mut chosen_path = path;

        while block <= last {
            // Evaluate liveness and the filter **once** per block, here, so the
            // path can be chosen from what the filter actually admitted rather
            // than from a separate counting pass. Both scan routines used to
            // evaluate it themselves, which duplicated the work and left the
            // planner nothing cheap to decide on.
            // `None` is a block with nothing to do; `Some( n )` is a visited
            // block that scored `n` documents. These used to be one `bool` plus
            // `block_hits.len()`, which silently coupled the reported statistic
            // to how many hits a kernel chose to keep -- so giving the forward
            // path a per-block top-k changed `scored` as a side effect.
            let outcome = (|| -> Result<Option<u64>> {
                // A retry may find this block newly empty or filtered out and
                // never enter a kernel. Discard the failed attempt's partial
                // hits before admission, not only when scoring starts.
                block_hits.clear();
                if !admission.live(&snap, block, &mut live)? {
                    return Ok(None);
                }
                admission.evaluate(&self.filter, &snap, block, &live, &mut cand)?;
                let admitted = crate::slice::popcount(&cand);
                if admitted == 0 {
                    return Ok(None);
                }
                let chosen = match self.hint {
                    PathHint::Auto => {
                        if u64::from(admitted) * crossover_for(m, meta.dims)
                            <= u64::from(crate::slice::popcount(&live))
                        {
                            Path::Gather
                        } else {
                            Path::Inverted
                        }
                    }
                    _ => path,
                };
                chosen_path = chosen;
                let ctx = BlockCtx {
                    floor,
                    block,
                    live: &live,
                    cand: &cand,
                };
                match chosen {
                    Path::Inverted => {
                        self.inverted_block(&snap, query, &ctx, &mut bufs, &mut block_hits)
                    }
                    Path::Gather | Path::DenseScan => {
                        self.forward_block(&snap, query, &ctx, chosen, &mut bufs, &mut block_hits)
                    }
                }
                .map(Some)
            })();
            match outcome {
                Ok(visited) => {
                    match visited {
                        Some(n) => {
                            stats.blocks_visited += 1;
                            scored += n;
                        }
                        None => stats.blocks_skipped += 1,
                    }
                    hits.append(&mut block_hits);
                    // **Truncate per block, not only at the end.** Blocks are
                    // scanned in ascending order and ties break on ascending id,
                    // so a later block can never displace an equal-scoring
                    // earlier document. Keeping `k` bounds the memory a long
                    // scan holds and makes the running threshold free to read.
                    if hits.len() > self.k {
                        hits.sort_by_key(Hit::rank_key);
                        hits.truncate(self.k);
                    }
                    floor = (hits.len() == self.k && self.k > 0).then(|| hits[self.k - 1].score);
                    block += 1;
                }
                Err(e) if e.is_snapshot_expired() => {
                    let retries = match self.consistency {
                        Consistency::ResumeOnEviction { max_retries } => max_retries,
                        Consistency::Strict => 0,
                    };
                    if stats.resumes >= retries {
                        return Err(e);
                    }
                    snap = self.index.store().snapshot()?;
                    // The cursor is bound to the snapshot that was evicted, so
                    // it goes with it. Reusing it would read the old version
                    // through lanes the new snapshot never opened.
                    bufs.reset_cursors();
                    admission.reopen(&snap)?;
                    vmin = vmin.min(snap.version());
                    vmax = vmax.max(snap.version());
                    stats.resumes += 1;
                    // `block` is not advanced: the failed block is redone in
                    // full against the new snapshot.
                }
                Err(e) => return Err(e),
            }
        }

        hits.sort_by_key(Hit::rank_key);
        hits.truncate(self.k);
        Ok(Hits {
            hits,
            // The path taken for the last block that had work. With `Auto` the
            // choice is per block, so this reports rather than characterises the
            // whole query -- which is why the differential suite forces a path
            // instead of reading this back.
            path: chosen_path,
            scored,
            version_range: (vmin, vmax),
            stats,
        })
    }
}

/// The last block that could hold a live document, or `None` if none could.
///
/// **`None` and `Some( 0 )` are different states**, and returning `0` for both is
/// what made a search against an empty index report a block it never visited:
/// `0 ..= 0` is one iteration, so every caller's loop ran once over a block that
/// does not exist. The `Option` makes the empty case unrepresentable as a range
/// rather than relying on each caller to remember it.
fn last_block(snap: &impl SetSnapshot, keys: crate::KeySpace) -> Result<Option<u64>> {
    Ok(snap.max(keys.live())?.map(crate::keyspace::block_of))
}

impl<S: SetStore> Search<'_, '_, S> {
    /// Read each admitted document's forward row and score it.
    ///
    /// Exact by construction: nothing is pruned and no bound is consulted.
    /// `Gather` and `DenseScan` differ only in whether a block's rows are
    /// visited through the admitted mask or through the live one; with the
    /// filter now evaluated per block they cost the same, and the distinction
    /// survives only so the differential suite can force each.
    /// The same scan, over several threads.
    ///
    /// # No shared mutable state on the hot path
    ///
    /// Each worker owns its buffers and its own result vector; the only thing
    /// shared is an atomic block counter and the snapshot, which is read-only.
    /// There is no shared threshold, because nothing here prunes across blocks
    /// yet -- when something does, it must be raise-only, so that a worker
    /// reading a stale lower value prunes *less* and stays exact.
    ///
    /// # Eviction retries the whole pass, not one block
    ///
    /// The serial scan redoes the block that failed. Here the snapshot is
    /// shared and immutable, so a worker cannot replace it for the others; the
    /// error propagates and the caller's retry re-runs the whole parallel pass
    /// against a fresh one. That redoes work an eviction interrupted, which is
    /// the price of not having each worker on its own version -- and a result
    /// assembled from several versions is a mixture no version ever held.
    fn scan_parallel(
        &self,
        query: CodeRef<'_>,
        last: u64,
        path: Path,
        snap: S::Snap,
    ) -> Result<Hits> {
        use std::sync::atomic::{AtomicU64, Ordering};

        let meta = self.index.meta();
        let m = weight_within(query, meta.dims);
        let max = match self.metric {
            Metric::Hamming => 2 * m + meta.dims,
            _ => m,
        };
        let levels = crate::slice::levels_for(max.max(1));
        let dims = query_dims(query, meta.dims);
        let keys = self.index.keys();

        let version = snap.version();
        let next = AtomicU64::new(0);
        // The best `k`-th score any worker has found. See the read site below
        // for why a lock rather than an atomic: `Score` is a rational, not a
        // `u64`, and the lock is taken once per block rather than per document.
        let shared_floor: std::sync::Mutex<Option<crate::Score>> = std::sync::Mutex::new(None);
        let shared_floor = &shared_floor;
        let workers = self.threads.min((last + 1) as usize);

        let results: Vec<Result<(Vec<Hit>, u64, ScanStats)>> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..workers)
                .map(|_| {
                    let (next, snap, dims) = (&next, &snap, &dims);
                    scope.spawn(move || {
                        let mut bufs = ScanBufs::new(levels, dims.clone());
                        // One cursor per worker, not one shared: a cursor is a
                        // position, and a shared position is either a lock on
                        // the hot path or a race. The snapshot stays shared --
                        // it is immutable, and one version is the whole point.
                        //
                        // Workers take blocks from a shared counter, so a given
                        // worker's blocks ascend without being contiguous. That
                        // is what a forward-only cursor needs and no more.
                        let mut admission = Admission::new(&self.filter, keys, snap)?;
                        let (mut live, mut cand) = (zero_mask(), zero_mask());
                        let mut hits: Vec<Hit> = Vec::new();
                        let mut block_hits: Vec<Hit> = Vec::new();
                        let mut stats = ScanStats::default();
                        let mut scored = 0u64;

                        loop {
                            let block = next.fetch_add(1, Ordering::Relaxed);
                            if block > last {
                                break;
                            }
                            if !admission.live(snap, block, &mut live)? {
                                stats.blocks_skipped += 1;
                                continue;
                            }
                            admission.evaluate(&self.filter, snap, block, &live, &mut cand)?;
                            let admitted = crate::slice::popcount(&cand);
                            if admitted == 0 {
                                stats.blocks_skipped += 1;
                                continue;
                            }
                            let chosen = match self.hint {
                                PathHint::Auto => {
                                    if u64::from(admitted) * crossover_for(m, meta.dims)
                                        <= u64::from(crate::slice::popcount(&live))
                                    {
                                        Path::Gather
                                    } else {
                                        Path::Inverted
                                    }
                                }
                                _ => path,
                            };
                            // **Shared, not per worker.** A per-worker floor
                            // was tried first and moved nothing: a corpus of
                            // 524 288 documents is eight blocks, so at eight
                            // threads every worker takes exactly one block and
                            // never has a previous one to learn from. The plan
                            // specified a shared threshold for this reason, and
                            // this is why.
                            //
                            // Raise-only, and a stale read is safe: a worker
                            // that sees an old, lower threshold prunes less and
                            // is still exact, because any `k`-th best found
                            // anywhere is a valid lower bound on the true one.
                            // So the lock is taken briefly, once per block, and
                            // never held across scoring.
                            let floor = *shared_floor.lock().expect("floor");
                            let ctx = BlockCtx {
                                floor,
                                block,
                                live: &live,
                                cand: &cand,
                            };
                            scored += match chosen {
                                Path::Inverted => self.inverted_block(
                                    snap,
                                    query,
                                    &ctx,
                                    &mut bufs,
                                    &mut block_hits,
                                )?,
                                Path::Gather | Path::DenseScan => self.forward_block(
                                    snap,
                                    query,
                                    &ctx,
                                    chosen,
                                    &mut bufs,
                                    &mut block_hits,
                                )?,
                            };
                            stats.blocks_visited += 1;
                            hits.append(&mut block_hits);
                            if hits.len() > self.k {
                                hits.sort_by_key(Hit::rank_key);
                                hits.truncate(self.k);
                            }
                            if let Some(mine) = (hits.len() >= self.k && self.k > 0).then(|| {
                                let mut top = hits.clone();
                                top.sort_by_key(Hit::rank_key);
                                top[self.k - 1].score
                            }) {
                                let mut g = shared_floor.lock().expect("floor");
                                if g.is_none_or(|cur| mine > cur) {
                                    *g = Some(mine);
                                }
                            }
                            // Keep only what can still place. Without this a
                            // worker holds every hit from every block it took.
                            if hits.len() > self.k.saturating_mul(4).max(64) {
                                hits.sort_by_key(Hit::rank_key);
                                hits.truncate(self.k);
                            }
                        }
                        hits.sort_by_key(Hit::rank_key);
                        hits.truncate(self.k);
                        Ok((hits, scored, stats))
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().expect("a scan worker panicked"))
                .collect()
        });

        let mut hits = Vec::new();
        let mut scored = 0u64;
        let mut stats = ScanStats::default();
        for r in results {
            let (h, s, st) = r?;
            hits.extend(h);
            scored += s;
            stats.blocks_visited += st.blocks_visited;
            stats.blocks_skipped += st.blocks_skipped;
        }
        hits.sort_by_key(Hit::rank_key);
        hits.truncate(self.k);
        Ok(Hits {
            hits,
            path,
            scored,
            version_range: (version, version),
            stats,
        })
    }

    /// Score one block through the forward rows. Returns whether it held
    /// anything live. `out` is cleared on entry, so an interrupted call
    /// contributes nothing.
    fn forward_block(
        &self,
        snap: &S::Snap,
        query: CodeRef<'_>,
        ctx: &BlockCtx<'_>,
        path: Path,
        b: &mut ScanBufs,
        out: &mut Vec<Hit>,
    ) -> Result<u64> {
        let (block, live, adm) = (ctx.block, *ctx.live, *ctx.cand);
        // See `inverted_block`: a retry redoes the whole block, so every buffer
        // the attempt touches is reset, not only the output.
        out.clear();

        let meta = self.index.meta();
        let keys = self.index.keys();
        let m = weight_within(query, meta.dims);

        // Liveness and the filter were evaluated once in `scan`, which is
        // also where the path was chosen from them. Re-deriving them here
        // would double the work and could disagree with the decision that
        // selected this path.

        let source = if path == Path::Gather { &adm } else { &live };
        crate::slice::offsets(source, &mut b.offs);
        let offs = std::mem::take(&mut b.offs);

        let use_heap = self.k > 0 && self.k <= FORWARD_HEAP_MAX_K;
        let mut heap = std::mem::take(&mut b.forward_heap);
        heap.clear();
        let mut scored = 0u64;
        let mut next = 0;
        while next < offs.len() {
            let off = offs[next];
            if path == Path::DenseScan && adm[off as usize >> 6] >> (off & 63) & 1 == 0 {
                next += 1;
                continue;
            }
            let id = DocId(block * crate::keyspace::BLOCK_ORDINALS + u64::from(off));
            let addr = self.index.row_addr(id);
            // Offsets are ascending. Group by the forward block, not by the
            // posting block: a forward block holds rows_per_block documents,
            // which need not divide the 65 536-document posting boundary.
            // One visit consumes every admitted row before its borrow expires.
            let end_id = (addr.block + 1) * u64::from(meta.rows_per_block);
            let start = next;
            while next < offs.len()
                && block * crate::keyspace::BLOCK_ORDINALS + u64::from(offs[next]) < end_id
            {
                next += 1;
            }
            let read = b.with_forward(snap, keys.forward(), addr.block, &mut |mask| {
                for &off in &offs[start..next] {
                    if path == Path::DenseScan && adm[off as usize >> 6] >> (off & 63) & 1 == 0 {
                        continue;
                    }
                    let id = DocId(block * crate::keyspace::BLOCK_ORDINALS + u64::from(off));
                    let row_addr = self.index.row_addr(id);
                    let lo = (row_addr.base % crate::keyspace::BLOCK_ORDINALS) as usize / 64;
                    let row = &mask[lo..lo + (meta.row_bits as usize) / 64];
                    let code = CodeRef::Dense(row);
                    let inter = query.intersect(code);
                    let weight = code.weight();
                    let hit = Hit {
                        id,
                        score: self.metric.score(inter, weight, m),
                        inter,
                        weight,
                    };
                    scored += 1;
                    if use_heap {
                        if heap.len() < self.k {
                            heap.push(RankedHit(hit));
                        } else if heap
                            .peek()
                            .is_some_and(|worst| hit.rank_key() < worst.0.rank_key())
                        {
                            heap.pop();
                            heap.push(RankedHit(hit));
                        }
                    } else {
                        out.push(hit);
                    }
                }
            });
            if let Err(error) = read {
                b.offs = offs;
                b.forward_heap = heap;
                return Err(error);
            }
        }
        b.offs = offs;
        if use_heap {
            out.extend(heap.iter().map(|ranked| ranked.0));
        } else if out.len() > self.k {
            // Large k: selecting once is cheaper than maintaining a large heap.
            out.select_nth_unstable_by_key(self.k, Hit::rank_key);
            out.truncate(self.k);
        }
        b.forward_heap = heap;
        Ok(scored)
    }

    /// Score one block through the inverted posting lists.
    ///
    /// # Why the ranking key is an integer, for the linear metrics
    ///
    /// Ranking by the intersection `a` alone is correct for `Dot` and **wrong**
    /// for Hamming, which depends on the document's own weight. But
    /// `-H = 2a + z - m - D` with `z = D - |x|`, and `m` and `D` are constants
    /// of the query and the index -- so maximizing the integer `S = 2a + z`
    /// maximizes `-H` exactly. Jaccard and cosine are ratios, are not exactly
    /// rankable this way, and go through the monotone-bound refinement instead.
    ///
    /// Merging blocks is exact: the top-k of a union is the top-k of the parts'
    /// top-k's, and every part is merged by the same total order.
    fn inverted_block(
        &self,
        snap: &S::Snap,
        query: CodeRef<'_>,
        ctx: &BlockCtx<'_>,
        b: &mut ScanBufs,
        out: &mut Vec<Hit>,
    ) -> Result<u64> {
        let (block, cand) = (ctx.block, *ctx.cand);
        // **Every buffer, not just `out`.** A block attempt may end anywhere --
        // the store can refuse any read -- and the retry redoes the block from
        // the start. Leaving the accumulator or the carry-save's pending
        // carries populated means the retry adds to the failed attempt's
        // residue, which produced intersections larger than the document's own
        // weight and an overflow assertion in `add_plane_at`.
        //
        // Discarding partial work is not confined to the visible output; it is
        // every piece of state the attempt touched.
        out.clear();
        b.csa.clear();
        b.acc.clear();

        let meta = self.index.meta();
        let keys = self.index.keys();
        let m = weight_within(query, meta.dims);
        let linear = matches!(self.metric, Metric::Dot | Metric::Hamming);

        b.open_lanes(snap, &keys, meta.z_planes())?;
        if self.metric == Metric::Hamming {
            b.z_masks.resize_with(meta.z_planes() as usize, zero_mask);
        }

        // Evaluated once in `scan`, which chose this path from it.
        let dims = std::mem::take(&mut b.dims);

        let shift = usize::from(self.metric == Metric::Hamming);
        let r = (|| -> Result<()> {
            if self.kernel == Kernel::Tiled && b.lanes.is_some() {
                let lane_count = dims.len()
                    + if self.metric == Metric::Hamming {
                        meta.z_planes() as usize
                    } else {
                        0
                    };
                let (lanes, scratch, acc, z_masks) = (
                    &mut b.lanes,
                    &mut b.lane_scratch,
                    &mut b.acc,
                    &mut b.z_masks,
                );
                lanes
                    .as_mut()
                    .expect("tiled cursor was opened")
                    .with_blocks(lane_count, block, scratch, &mut |view| {
                        slice::tiled_into(view, dims.len(), lane_count - dims.len(), shift, acc);
                        for (j, mask) in z_masks.iter_mut().enumerate() {
                            mask.copy_from_slice(view.lane(dims.len() + j));
                        }
                    })?;
            } else {
                match self.kernel {
                    Kernel::Tiled | Kernel::CarrySave => {
                        for (lane, &d) in dims.iter().enumerate() {
                            if read_lane(
                                snap,
                                &mut b.lanes,
                                lane,
                                keys.dim(d),
                                block,
                                &mut b.scratch,
                            )? {
                                b.csa.push_at(&b.scratch, shift);
                            }
                        }
                        b.csa.finish_into(&mut b.acc);
                    }
                    Kernel::Ripple => {
                        for (lane, &d) in dims.iter().enumerate() {
                            if read_lane(
                                snap,
                                &mut b.lanes,
                                lane,
                                keys.dim(d),
                                block,
                                &mut b.scratch,
                            )? {
                                b.acc.add_plane_at(&b.scratch, shift);
                            }
                        }
                    }
                }
                if self.metric == Metric::Hamming {
                    for j in 0..meta.z_planes() {
                        let lane = dims.len() + j as usize;
                        if read_lane(
                            snap,
                            &mut b.lanes,
                            lane,
                            keys.zplane(j),
                            block,
                            &mut b.scratch,
                        )? {
                            b.acc.add_plane_at(&b.scratch, j as usize);
                        }
                        b.z_masks[j as usize].copy_from_slice(&b.scratch);
                    }
                }
            }
            Ok(())
        })();
        b.dims = dims;
        r?;
        let acc = &b.acc;

        // A block with statistics gets the tightened bound; one without gets the
        // loose one. Never the other way round: statistics are deleted by any
        // write that touches the block, so their absence is the safe state.
        let w_min =
            crate::BlockStats::from_ordinals(&snap.load(keys.stat(block))?).map_or(0, |s| s.w_min);

        let picked = slice::top_k(acc, &cand, self.k);
        let selected = if linear {
            // Only the lowest-ID members of the boundary tie can reach this
            // block's exact top-k. Scoring the rest repeats weight lookups for
            // hits that the block merge must discard.
            let mut both = picked.confirmed;
            let mut remaining = self.k.saturating_sub(slice::popcount(&both) as usize);
            for (slot, tied) in both.iter_mut().zip(picked.tied.iter()) {
                let mut bits = *tied;
                while bits != 0 && remaining != 0 {
                    let first = bits & bits.wrapping_neg();
                    *slot |= first;
                    bits &= bits - 1;
                    remaining -= 1;
                }
            }
            both
        } else {
            let r = Refine {
                metric: self.metric,
                m,
                w_min,
                k: self.k,
            };
            refine(acc, &cand, &picked, &r, ctx.floor, |off| {
                let id = DocId(block * crate::keyspace::BLOCK_ORDINALS + u64::from(off));
                weight_of(snap, keys, meta, id)
            })?
        };

        let mut offs = std::mem::take(&mut b.offs);
        slice::offsets(&selected, &mut offs);
        for &off in &offs {
            let id = DocId(block * crate::keyspace::BLOCK_ORDINALS + u64::from(off));
            let v = acc.value_at(off);
            let weight = if self.metric == Metric::Hamming {
                let z = b.z_masks.iter().enumerate().fold(0u32, |z, (j, mask)| {
                    z | (((mask[(off as usize) >> 6] >> (off & 63)) & 1) as u32) << j
                });
                meta.dims - z
            } else {
                weight_of(snap, keys, meta, id)?
            };
            let inter = if self.metric == Metric::Hamming {
                (v - (meta.dims - weight)) / 2
            } else {
                v
            };
            out.push(Hit {
                id,
                score: self.metric.score(inter, weight, m),
                inter,
                weight,
            });
        }
        b.offs = offs;
        // Every hit here had its exact score computed, and nothing else did:
        // the descent and the refinement rejected the rest without scoring them,
        // which is the whole economy of this path and is what `scored` reports.
        Ok(out.len() as u64)
    }
}

/// The set bits of a query code, as dimension indices.
fn query_dims(query: CodeRef<'_>, dims: u32) -> Vec<u32> {
    let mut v = Vec::new();
    match query {
        CodeRef::Dense(words) => {
            for (wi, word) in words.iter().enumerate() {
                let mut x = *word;
                while x != 0 {
                    let b = (wi as u32) * 64 + x.trailing_zeros();
                    if b < dims {
                        v.push(b);
                    }
                    x &= x - 1;
                }
            }
        }
        CodeRef::Sparse(pos) => v.extend(pos.iter().copied().filter(|&b| b < dims)),
    }
    v
}

/// A document's weight, read from the stored complement planes.
fn weight_of<S: SetSnapshot>(
    snap: &S,
    keys: crate::KeySpace,
    meta: crate::IndexMeta,
    id: DocId,
) -> Result<u32> {
    let mut z = 0u32;
    for j in 0..meta.z_planes() {
        if snap.contains(keys.zplane(j), id.get())? {
            z |= 1 << j;
        }
    }
    Ok(meta.dims - z)
}

/// Widen a provisional pick into every ordinal that could still reach the top-k.
///
/// # Why a second pass, and why it is exact
///
/// Jaccard and cosine are ratios of the intersection to something that varies
/// per document, so ordering by intersection is not ordering by score. What
/// holds is that each is bounded above by a **monotone** function of the
/// intersection alone ( [`Metric::bound`] ), and that bound has an exact inverse
/// ( [`Metric::min_intersection`] ).
///
/// So: score the descent's provisional pick exactly and take its `k`-th value
/// as `tau`. For **any** set of `k` documents, its `k`-th best score is at most
/// the true `k`-th best, so `tau` is a valid lower bound. Everything with
/// `a < min_intersection(tau)` has score at most `bound(a) < tau` and therefore
/// cannot be in the top-k, so `{ a >= min_intersection(tau) }` is a superset of
/// the true top-k. Scoring all of it exactly is exact.
///
/// One pass, not a loop. Iterating would raise `tau` and shrink the candidate
/// set further, which is faster and no more correct; it is an optimization and
/// waits for a measurement saying the second pass is wide in practice.
/// What the refinement needs to know about the query and the block.
struct Refine {
    metric: Metric,
    /// The query weight.
    m: u32,
    /// A lower bound on every candidate's weight, from the block statistics, or
    /// zero when the block has none.
    w_min: u32,
    k: usize,
}

fn refine<E>(
    acc: &crate::slice::Slice,
    cand: &crate::slice::BlockMask,
    picked: &crate::slice::TopK,
    r: &Refine,
    floor: Option<crate::Score>,
    mut weight: impl FnMut(u32) -> std::result::Result<u32, E>,
) -> std::result::Result<crate::slice::BlockMask, E> {
    let Refine {
        metric,
        m,
        w_min,
        k,
    } = *r;
    if k == 0 {
        return Ok(crate::slice::zero_mask());
    }
    let mut provisional = picked.confirmed;
    for (slot, tied) in provisional.iter_mut().zip(picked.tied.iter()) {
        *slot |= *tied;
    }

    let mut offs = Vec::new();
    crate::slice::offsets(&provisional, &mut offs);
    let mut scores = Vec::with_capacity(offs.len());
    for &off in &offs {
        let a = acc.value_at(off);
        scores.push(metric.score(a, weight(off)?, m));
    }
    // Fewer than `k` candidates means this block offers no threshold of its
    // own. It does not mean there is none: a floor from earlier blocks still
    // applies, and a block too small to rank itself is exactly where an
    // external threshold is worth the most.
    let local = if scores.len() < k {
        None
    } else {
        scores.sort_unstable_by(|a, b| b.cmp(a));
        Some(scores[k - 1])
    };
    let tau = match (local, floor) {
        (Some(a), Some(b)) => a.max(b),
        (Some(a), None) => a,
        (None, Some(b)) => b,
        (None, None) => return Ok(*cand),
    };

    let need = metric.min_intersection_with(tau, m, w_min);
    Ok(crate::slice::ge(acc, cand, need))
}

#[cfg(test)]
mod refine_tests {
    use super::*;

    /// Build an accumulator whose value at ordinal `i` is `values[i]`, and a
    /// candidate mask over the same range.
    fn acc_of(values: &[u32]) -> (crate::slice::Slice, BlockMask) {
        let levels = crate::slice::levels_for(values.iter().copied().max().unwrap_or(0).max(1));
        let mut acc = crate::slice::Slice::new(levels);
        let mut cand = zero_mask();
        for j in 0..levels {
            let mut plane = zero_mask();
            for (i, v) in values.iter().enumerate() {
                if v >> j & 1 == 1 {
                    plane[i >> 6] |= 1u64 << (i & 63);
                }
            }
            acc.add_plane_at(&plane, j);
        }
        for i in 0..values.len() {
            cand[i >> 6] |= 1u64 << (i & 63);
        }
        (acc, cand)
    }

    /// Applying the refinement to its own output changes nothing.
    ///
    /// # Why this is the answer to "should the refinement iterate"
    ///
    /// The design sketch had the refinement as a **loop**: score the survivors,
    /// raise `tau`, shrink, repeat. The backlog carried that as an open
    /// optimization for three milestones, deferred on the grounds that a
    /// measurement had not shown the second pass would be wide.
    ///
    /// It cannot be, and no measurement is needed. The descent picks the top `k`
    /// by intersection; `tau` is the `k`-th **exact** score among those; and the
    /// survivor set keeps everything whose intersection could still reach `tau`
    /// -- which necessarily includes every one of those same top `k`, because
    /// the one that achieved `tau` has `bound(a) >= tau` by definition of a
    /// bound. So a second descent over the survivors returns the same `k`
    /// documents, computes the same `tau`, and keeps the same set. **One pass is
    /// already the fixed point.**
    ///
    /// The sketch's loop is not this loop: its step scores *every* survivor
    /// rather than the top `k`, which does raise `tau` and is exactly the cost
    /// the refinement exists to avoid. Iterating what is implemented is free and
    /// buys nothing; iterating what was designed buys something and is not
    /// cheap. Both readings close the item.
    #[test]
    fn a_second_refinement_pass_is_a_fixed_point() {
        // Weights chosen so the ranking by exact score differs from the ranking
        // by intersection -- otherwise the bound is trivially tight and the test
        // proves nothing about the refinement.
        let values = [9u32, 8, 8, 7, 6, 6, 5, 4, 3, 2, 1, 0];
        let weights = [40u32, 9, 30, 8, 7, 25, 6, 5, 4, 3, 2, 1];
        let (acc, cand) = acc_of(&values);

        for metric in [Metric::Jaccard, Metric::Cosine] {
            for k in [1usize, 3, 5] {
                for w_min in [0u32, 1, 5] {
                    let r = Refine {
                        metric,
                        m: 10,
                        w_min,
                        k,
                    };
                    let w =
                        |off: u32| -> std::result::Result<u32, ()> { Ok(weights[off as usize]) };

                    let picked1 = crate::slice::top_k(&acc, &cand, k);
                    let first = refine(&acc, &cand, &picked1, &r, None, w).expect("first");

                    let picked2 = crate::slice::top_k(&acc, &first, k);
                    let second = refine(&acc, &first, &picked2, &r, None, w).expect("second");

                    assert_eq!(
                        first, second,
                        "{metric:?} k={k} w_min={w_min}: a second pass changed the survivors"
                    );
                    // And the survivors must still contain the documents the
                    // first descent picked, which is why the fixed point holds.
                    for (a, b) in first.iter().zip(picked1.confirmed.iter()) {
                        assert_eq!(a & b, *b, "a confirmed document was dropped");
                    }
                }
            }
        }
    }

    /// A threshold from **elsewhere** narrows what a block's own cannot.
    ///
    /// This is the thing the fixed-point result above leaves room for, and the
    /// reason the scan carries a floor between blocks. A block's own top-`k`
    /// gives the best threshold available from inside it, and by the argument
    /// above that threshold has already done all it can. A `k`-th best score
    /// found in an *earlier* block is a different quantity, is an equally valid
    /// lower bound on the true `k`-th best, and can be strictly higher.
    ///
    /// Measured on 524 288 documents at D=256: carrying it cut documents given
    /// an exact score from 126 160 to 67 672 on arbitrarily ordered ids, and
    /// from 3 136 to **358** on weight-ordered ones.
    #[test]
    fn a_floor_from_an_earlier_block_narrows_further() {
        let values = [9u32, 8, 8, 7, 6, 6, 5, 4, 3, 2, 1, 0];
        let weights = [40u32, 9, 30, 8, 7, 25, 6, 5, 4, 3, 2, 1];
        let (acc, cand) = acc_of(&values);
        let r = Refine {
            metric: Metric::Jaccard,
            m: 10,
            w_min: 0,
            k: 3,
        };
        let w = |off: u32| -> std::result::Result<u32, ()> { Ok(weights[off as usize]) };
        let picked = crate::slice::top_k(&acc, &cand, 3);

        let without = refine(&acc, &cand, &picked, &r, None, w).expect("no floor");
        // A floor at the best score any document here achieves: nothing can beat
        // it, so the survivors must collapse to those that could tie it.
        let best = (0..values.len() as u32)
            .map(|o| Metric::Jaccard.score(acc.value_at(o), weights[o as usize], 10))
            .max()
            .expect("a score");
        let with = refine(&acc, &cand, &picked, &r, Some(best), w).expect("floor");

        assert!(
            crate::slice::popcount(&with) < crate::slice::popcount(&without),
            "an external floor at the block's own best score narrowed nothing: \
             {} against {}",
            crate::slice::popcount(&with),
            crate::slice::popcount(&without)
        );
        // A floor below the local threshold must change nothing, or the scan
        // would be pruning on the weaker of the two bounds.
        let weak = Metric::Jaccard.score(0, 1, 10);
        let with_weak = refine(&acc, &cand, &picked, &r, Some(weak), w).expect("weak");
        assert_eq!(with_weak, without, "a weaker floor displaced the local one");
    }

    /// The refinement narrows something, so the test above is not vacuous.
    #[test]
    fn the_refinement_actually_removes_candidates() {
        let values = [9u32, 8, 8, 7, 6, 6, 5, 4, 3, 2, 1, 0];
        let weights = [40u32, 9, 30, 8, 7, 25, 6, 5, 4, 3, 2, 1];
        let (acc, cand) = acc_of(&values);
        let r = Refine {
            metric: Metric::Jaccard,
            m: 10,
            w_min: 0,
            k: 3,
        };
        let picked = crate::slice::top_k(&acc, &cand, 3);
        let out = refine(&acc, &cand, &picked, &r, None, |off: u32| {
            std::result::Result::<u32, ()>::Ok(weights[off as usize])
        })
        .expect("refine");
        assert!(
            crate::slice::popcount(&out) < crate::slice::popcount(&cand),
            "the refinement kept every candidate, so the fixed-point test proves nothing"
        );
    }
}
