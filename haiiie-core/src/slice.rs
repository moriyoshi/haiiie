//! Bit-sliced integers over a block, and exact top-k by descending them.
//!
//! # The lens
//!
//! `L` planes of 65 536 bits each represent one unsigned integer per ordinal:
//! `value(o) = sum over j of 2^j * [ bit o of plane j ]`. Addition is
//! ripple-carry over whole planes, so **one plane operation adds a bit to all
//! 65 536 accumulators at once**. That is the entire reason the inverted path
//! can be competitive: a posting list is already exactly the addend.
//!
//! This arithmetic is set algebra with no notion of similarity in it.
//! A bit-sliced lens was proposed to yesnodb on 2026-09-13; the proposal
//! closed on 2026-09-19 after measurement and review. We withdrew three
//! parts, and upstream declined the fourth. This module is the implementation
//! haiiie uses. Its benchmark measures block operations in these local plane
//! buffers, not yesnodb container operations; constants here are derived
//! rather than tuned.
//!
//! # Ripple-carry, deliberately, and it is the slow one
//!
//! Adding a one-bit addend costs two operations per plane -- `carry = acc &
//! addend; acc ^= addend` -- so `2L` per addend, against carry-save's five
//! operations independent of `L`.
//!
//! **That ratio is an operation count and not a speedup, which this block used
//! to imply.** Measured over one block in these buffers, carry-save runs 1.09x
//! faster at 32 addends, 1.26x at 128 and 1.52x at 512, where `2L / 5` predicts
//! 2.4x, 3.2x and 4.0x. The model counts instructions and says nothing about
//! memory traffic, vectorization, or the `O(L^2)` fold carry-save pays once per
//! block. A third fitted constant would not repair it; the family of models is
//! wrong for the quantity.
//!
//! Ripple stays **because it is the oracle for that kernel**: two
//! implementations that must agree bit for bit, where only the slow one is
//! obviously correct. The fast one landed; do not delete this.
//!
//! # Word-tiled accumulation
//!
//! The default inverted path now borrows all posting lanes for a block and
//! accumulates sixteen adjacent words at a time. A Harley-Seal tree compresses
//! groups of sixteen dimension lanes; ripple folds only its carry, leftovers
//! and Hamming weight planes. In the 1,048,576-document D=256 persisted probe,
//! 20 Hamming queries over 16 blocks, copied-lane kernel time was 0.735 ms per
//! query against 1.694 ms for plane-wide CSA plus Z planes. That 2.30x ratio is
//! inside the arithmetic kernel; the whole query also pays lane lookup and
//! admission. Plane-wide [`Csa`] and [`Slice::add_plane_at`] remain forced
//! oracles for the tiled result.

use crate::keyspace::BLOCK_ORDINALS;

/// `u64` words in one block's bit plane.
pub const BLOCK_WORDS: usize = (BLOCK_ORDINALS / 64) as usize;

/// One block-wide bit plane.
pub type BlockMask = [u64; BLOCK_WORDS];

/// An all-zero plane.
#[must_use]
pub const fn zero_mask() -> BlockMask {
    [0u64; BLOCK_WORDS]
}

/// How many planes an unsigned value up to `max` needs.
#[must_use]
pub fn levels_for(max: u32) -> usize {
    (u32::BITS - max.leading_zeros()) as usize
}

/// Population count of a mask.
#[must_use]
pub fn popcount(m: &BlockMask) -> u32 {
    m.iter().map(|w| w.count_ones()).sum()
}

/// A bit-sliced unsigned integer per ordinal, over one block.
#[derive(Clone, Debug)]
pub struct Slice {
    planes: Vec<BlockMask>,
}

impl Slice {
    /// Mutable access to the planes, for in-place fills.
    fn planes_mut(&mut self) -> &mut [BlockMask] {
        &mut self.planes
    }
}

impl Slice {
    /// A slice of `levels` planes, all zero.
    #[must_use]
    pub fn new(levels: usize) -> Self {
        Self {
            planes: vec![zero_mask(); levels.max(1)],
        }
    }

    /// Reset to zero without reallocating. The scan reuses one slice across
    /// every block, so this is on the hot path and must not allocate.
    pub fn clear(&mut self) {
        for p in &mut self.planes {
            p.fill(0);
        }
    }

    /// How many planes.
    #[must_use]
    pub fn levels(&self) -> usize {
        self.planes.len()
    }

    /// The planes, most significant last.
    #[must_use]
    pub fn planes(&self) -> &[BlockMask] {
        &self.planes
    }

    /// Add a one-bit addend weighted `2^shift` to every ordinal it marks.
    ///
    /// This is the whole arithmetic. A query dimension's posting list enters at
    /// `shift = 0` for a plain intersection count, or `shift = 1` when the
    /// ranking key is `2a + z`; a stored weight plane `j` enters at
    /// `shift = j`. Nothing else is needed to build either quantity.
    ///
    /// Values saturate rather than wrap: a carry out of the top plane is
    /// dropped. Callers size the slice with [`levels_for`] so it cannot happen,
    /// and `debug_assert` catches a caller that did not.
    ///
    /// # Level-major, and the loop order is the measurement
    ///
    /// The inner loop runs over **words within a level**, not levels within a
    /// word, and the early exit is checked once per level rather than once per
    /// word. The word-major arrangement reads more naturally and is **2.3x
    /// slower** at 128 and 512 addends ( `benches/accumulate.rs` ): a
    /// per-word data-dependent branch prevents the level from vectorizing, and
    /// at density 0.5 that branch almost never fires anyway, so it costs
    /// everything and saves nothing.
    ///
    /// This was found by a benchmark arm that had no business winning --
    /// `composed-allocating`, which allocates two vectors per level per addend,
    /// was beating this function. Allocation cost cannot explain that and loop
    /// order can.
    pub fn add_plane_at(&mut self, addend: &BlockMask, shift: usize) {
        debug_assert!(shift < self.planes.len(), "addend weight exceeds the slice");
        let mut carry = *addend;
        for plane in &mut self.planes[shift..] {
            let mut any = 0u64;
            for w in 0..BLOCK_WORDS {
                let t = plane[w] & carry[w];
                plane[w] ^= carry[w];
                carry[w] = t;
                any |= t;
            }
            if any == 0 {
                return;
            }
        }
        debug_assert!(
            carry.iter().all(|w| *w == 0),
            "bit-sliced value overflowed its planes"
        );
    }

    /// Add another slice, weighted by `2^shift`.
    ///
    /// Plane `j` of `other` has weight `2^j`, so it enters here at `shift + j`.
    /// Used to form `S = 2a + z` from a separately accumulated `a`: `L` ripple
    /// additions rather than one per query dimension.
    pub fn add_slice_at(&mut self, other: &Slice, shift: usize) {
        for (j, plane) in other.planes.iter().enumerate() {
            if plane.iter().any(|w| *w != 0) {
                self.add_plane_at(plane, shift + j);
            }
        }
    }

    /// The value at one ordinal offset within the block.
    #[must_use]
    pub fn value_at(&self, offset: u32) -> u32 {
        let (w, b) = (offset as usize >> 6, offset & 63);
        let mut v = 0u32;
        for (j, plane) in self.planes.iter().enumerate() {
            v |= (((plane[w] >> b) & 1) as u32) << j;
        }
        v
    }
}

/// A Harley-Seal carry-save accumulator.
///
/// # What it buys, and the arithmetic behind the claim
///
/// Ripple-carry adds a one-bit addend with two operations per plane -- `carry =
/// acc & addend; acc ^= addend` -- so **`2L` per addend**, growing with the
/// accumulator width. A carry-save adder takes three one-bit inputs and emits
/// two outputs of different weight in **five** operations:
///
/// ```text
/// sum   = a ^ b ^ c
/// carry = (a & b) | (c & (a ^ b))     // a + b + c == 2*carry + sum
/// ```
///
/// Feeding it the accumulator plane plus **two** new addends consumes two
/// addends per call. Level 0 therefore runs `n/2` calls, level 1 `n/4`, and so
/// on, for `5n/2 * (1 + 1/2 + 1/4 + ...) = 5n` operations total -- **five per
/// addend, independent of `L`**. The ratio against ripple is `2L/5`: 3.2x at
/// `L = 8`, 4.0x at `L = 10`.
///
/// Those are derived operation counts, not measurements. At container
/// granularity the word-operation model is the wrong one anyway -- allocation,
/// cardinality bookkeeping and kind dispatch dominate -- which is why the
/// benchmark measures passes and allocations rather than checking this figure.
///
/// # It is not the oracle
///
/// [`Slice::add_plane_at`] is, and stays. Two implementations that must agree
/// bit for bit, where only the slow one is obviously correct.
#[derive(Clone, Debug)]
pub struct Csa {
    planes: Vec<BlockMask>,
    /// At most one unpaired addend per level, waiting for a partner.
    pending: Vec<Option<BlockMask>>,
}

impl Csa {
    /// An accumulator wide enough for a value up to `max`.
    #[must_use]
    pub fn new(levels: usize) -> Self {
        let n = levels.max(1);
        Self {
            planes: vec![zero_mask(); n],
            pending: vec![None; n + 1],
        }
    }

    /// Reset without reallocating.
    pub fn clear(&mut self) {
        for p in &mut self.planes {
            p.fill(0);
        }
        for p in &mut self.pending {
            *p = None;
        }
    }

    /// Add a one-bit addend of weight `2^shift`.
    pub fn push_at(&mut self, addend: &BlockMask, shift: usize) {
        let mut level = shift;
        let mut carried;
        let mut current = *addend;
        loop {
            debug_assert!(level < self.pending.len(), "carry escaped the accumulator");
            match self.pending[level].take() {
                None => {
                    self.pending[level] = Some(current);
                    return;
                }
                Some(held) => {
                    // Three inputs at this weight: the accumulator plane and two
                    // addends. Emit the sum here and one carry one level up.
                    let mut carry = zero_mask();
                    if level < self.planes.len() {
                        let plane = &mut self.planes[level];
                        for w in 0..BLOCK_WORDS {
                            let (a, u) = (plane[w], held[w] ^ current[w]);
                            carry[w] = (held[w] & current[w]) | (a & u);
                            plane[w] = a ^ u;
                        }
                    } else {
                        // Past the top plane: the sum is dropped, which cannot
                        // happen for a correctly sized accumulator.
                        debug_assert!(
                            held.iter().all(|w| *w == 0) && current.iter().all(|w| *w == 0),
                            "bit-sliced value overflowed its planes"
                        );
                        for w in 0..BLOCK_WORDS {
                            carry[w] = held[w] & current[w];
                        }
                    }
                    carried = carry;
                }
            }
            if carried.iter().all(|w| *w == 0) {
                return;
            }
            current = carried;
            level += 1;
        }
    }

    /// Fold the held addends into `out` and reset, allocating nothing.
    ///
    /// Each pending entry is one un-paired addend of weight `2^j`, so it goes in
    /// by ordinary ripple -- at most one per level, so this is `O(L^2)` word
    /// operations once per block rather than per addend.
    ///
    /// Takes `&mut self` and a caller-owned `out` rather than consuming and
    /// returning, so a scan reuses one accumulator and one slice across every
    /// block. The consuming shape reads better and costs an allocation per
    /// block, which is precisely what the allocation budget exists to refuse.
    pub fn finish_into(&mut self, out: &mut Slice) {
        debug_assert_eq!(out.levels(), self.planes.len(), "slice width mismatch");
        for (dst, src) in out.planes_mut().iter_mut().zip(self.planes.iter()) {
            dst.copy_from_slice(src);
        }
        for (j, held) in self.pending.iter().enumerate() {
            if let Some(mask) = held
                && j < out.levels()
            {
                out.add_plane_at(mask, j);
            }
        }
        self.clear();
    }

    /// Fold the held addends in and yield the accumulated slice.
    ///
    /// Convenience for tests and one-shot use. The scan uses
    /// [`Csa::finish_into`], which allocates nothing.
    #[must_use]
    pub fn finish(mut self) -> Slice {
        let mut out = Slice::new(self.planes.len());
        self.finish_into(&mut out);
        out
    }
}

// Sixteen adjacent words are the measured tile width: on the persisted
// 1,048,576-row D=256 Hamming fixture, 20 queries x 16 blocks, the copied-lane
// kernel took 0.735 ms/query versus 1.694 ms for plane-wide CSA plus Z planes.
const TILE_WORDS: usize = 16;
// `levels_for(u32::MAX)` is 32; every stored score is bounded by u32.
const TILE_LEVELS: usize = u32::BITS as usize;
type Tile = [u64; TILE_WORDS];

#[inline(always)]
fn tile_csa(a: &Tile, b: &Tile, c: &Tile) -> (Tile, Tile) {
    let mut hi = [0; TILE_WORDS];
    let mut lo = [0; TILE_WORDS];
    for w in 0..TILE_WORDS {
        let u = a[w] ^ b[w];
        lo[w] = u ^ c[w];
        hi[w] = (a[w] & b[w]) | (u & c[w]);
    }
    (hi, lo)
}

#[inline(always)]
fn tile_ripple(count: &mut [Tile], addend: &Tile, shift: usize) {
    if shift >= count.len() {
        debug_assert!(addend.iter().all(|word| *word == 0));
        return;
    }
    let mut carry = *addend;
    for plane in &mut count[shift..] {
        let mut any = 0;
        for w in 0..TILE_WORDS {
            let next = plane[w] & carry[w];
            plane[w] ^= carry[w];
            carry[w] = next;
            any |= next;
        }
        if any == 0 {
            return;
        }
    }
    debug_assert!(
        carry.iter().all(|word| *word == 0),
        "tiled slice overflowed"
    );
}

/// Fill `out` with exact counts from simultaneously visible posting lanes.
///
/// Dimension lanes contribute at `shift` (zero for intersection count, one
/// for Hamming's `2a + z`); the following Z lanes contribute at their own
/// successive levels. Every tile is written fully, so no per-block mask copy
/// or per-tile heap allocation is needed. CSA and ripple remain the oracles.
pub(crate) fn tiled_into(
    lanes: &dyn crate::store::BlockLanes,
    dimensions: usize,
    z_planes: usize,
    shift: usize,
    out: &mut Slice,
) {
    debug_assert_eq!(lanes.len(), dimensions + z_planes);
    if let Some(masks) = lanes.as_refs() {
        tiled_with(|lane| masks[lane], dimensions, z_planes, shift, out);
    } else {
        tiled_with(|lane| lanes.lane(lane), dimensions, z_planes, shift, out);
    }
}

#[inline]
fn tiled_with<'a>(
    mask: impl Fn(usize) -> &'a BlockMask,
    dimensions: usize,
    z_planes: usize,
    shift: usize,
    out: &mut Slice,
) {
    debug_assert!(out.levels() <= TILE_LEVELS);
    for base in (0..BLOCK_WORDS).step_by(TILE_WORDS) {
        let load = |lane: usize| -> Tile {
            mask(lane)[base..base + TILE_WORDS]
                .try_into()
                .expect("one complete word tile")
        };
        let (mut ones, mut twos, mut fours, mut eights) = (
            [0; TILE_WORDS],
            [0; TILE_WORDS],
            [0; TILE_WORDS],
            [0; TILE_WORDS],
        );
        let mut count = [[0; TILE_WORDS]; TILE_LEVELS];
        let active = &mut count[..out.levels()];
        let mut lane = 0;
        while lane + 16 <= dimensions {
            let (ta, o) = tile_csa(&ones, &load(lane), &load(lane + 1));
            ones = o;
            let (tb, o) = tile_csa(&ones, &load(lane + 2), &load(lane + 3));
            ones = o;
            let (fa, t) = tile_csa(&twos, &ta, &tb);
            twos = t;
            let (ta, o) = tile_csa(&ones, &load(lane + 4), &load(lane + 5));
            ones = o;
            let (tb, o) = tile_csa(&ones, &load(lane + 6), &load(lane + 7));
            ones = o;
            let (fb, t) = tile_csa(&twos, &ta, &tb);
            twos = t;
            let (ea, f) = tile_csa(&fours, &fa, &fb);
            fours = f;
            let (ta, o) = tile_csa(&ones, &load(lane + 8), &load(lane + 9));
            ones = o;
            let (tb, o) = tile_csa(&ones, &load(lane + 10), &load(lane + 11));
            ones = o;
            let (fa, t) = tile_csa(&twos, &ta, &tb);
            twos = t;
            let (ta, o) = tile_csa(&ones, &load(lane + 12), &load(lane + 13));
            ones = o;
            let (tb, o) = tile_csa(&ones, &load(lane + 14), &load(lane + 15));
            ones = o;
            let (fb, t) = tile_csa(&twos, &ta, &tb);
            twos = t;
            let (eb, f) = tile_csa(&fours, &fa, &fb);
            fours = f;
            let (sixteens, e) = tile_csa(&eights, &ea, &eb);
            eights = e;
            tile_ripple(active, &sixteens, shift + 4);
            lane += 16;
        }
        for i in lane..dimensions {
            tile_ripple(active, &load(i), shift);
        }
        tile_ripple(active, &ones, shift);
        tile_ripple(active, &twos, shift + 1);
        tile_ripple(active, &fours, shift + 2);
        tile_ripple(active, &eights, shift + 3);
        for j in 0..z_planes {
            tile_ripple(active, &load(dimensions + j), j);
        }
        for (plane, words) in out.planes_mut().iter_mut().zip(active.iter()) {
            plane[base..base + TILE_WORDS].copy_from_slice(words);
        }
    }
}

/// The outcome of a descent: what is certainly in the top-k, and the tie class
/// at the boundary.
#[derive(Clone, Debug)]
pub struct TopK {
    /// Certainly in the top-k. At most `k` ordinals.
    pub confirmed: BlockMask,
    /// All tied at the `k`-th value. Take `k - |confirmed|` of them, by any
    /// rule the caller states; this crate's rule is ascending ordinal.
    pub tied: BlockMask,
}

/// Exact top-`k` by bit-sliced value, within `within`.
///
/// Walks the planes from the most significant, narrowing when the candidates
/// with the current bit set still exceed `k` and confirming them when they do
/// not. Each step needs only a population count, never a materialized value --
/// which is why this is worth doing rather than extracting every score.
///
/// Two details that are easy to get wrong and are both load-bearing: the
/// comparison against `k` must be **strict** ( at equality the confirming branch
/// is the correct one ), and the loop needs an explicit exit once `k` is
/// confirmed, or it leaves an irrelevant tie class behind.
///
/// Deliberately **does not** return the k-th value. Deriving it from the branch
/// taken at each plane is wrong in exactly the case where the confirming branch
/// reaches `k`, and a cross-block pruning threshold is not needed until block
/// bounds exist. An approximately-right threshold silently drops results.
#[must_use]
pub fn top_k(slice: &Slice, within: &BlockMask, k: usize) -> TopK {
    let mut confirmed = zero_mask();
    let mut cand = *within;
    let mut n = 0usize;

    for plane in slice.planes().iter().rev() {
        if n >= k {
            cand = zero_mask();
            break;
        }
        let mut hit = zero_mask();
        for w in 0..BLOCK_WORDS {
            hit[w] = cand[w] & plane[w];
        }
        let hc = popcount(&hit) as usize;
        if n + hc > k {
            cand = hit;
        } else {
            for w in 0..BLOCK_WORDS {
                confirmed[w] |= hit[w];
                cand[w] &= !plane[w];
            }
            n += hc;
        }
    }
    TopK {
        confirmed,
        tied: cand,
    }
}

/// `{ o in within : value(o) >= t }`, as a mask.
///
/// A descending compare: walk the planes from the most significant, keeping a
/// set that is still equal to `t` so far and a set already known to exceed it.
/// Where `t`'s bit is 0, any candidate with a 1 there is strictly greater and
/// graduates; the rest stay equal only if their bit is also 0. Where `t`'s bit
/// is 1, only candidates with a 1 stay in contention. Whatever is still equal
/// at the end is equal to `t`, which satisfies `>=`.
///
/// `2L` plane operations, and it materializes one mask rather than any scores --
/// which is the point. The refinement pass uses it to discard everything that
/// could not reach the running threshold, without computing what any of them
/// would have scored.
#[must_use]
pub fn ge(slice: &Slice, within: &BlockMask, t: u32) -> BlockMask {
    // A threshold above everything the slice can represent selects nothing.
    // Without this the loop sees only `t`'s low `L` bits and reads `t = 2^L` as
    // zero, selecting **everything** -- which is conservative enough to keep a
    // refinement pass exact, and would therefore have shown up as pruning that
    // silently never happened rather than as a wrong answer. Found by sweeping
    // one threshold past the representable range.
    let levels = slice.levels();
    if levels < u32::BITS as usize && t >= (1u32 << levels) {
        return zero_mask();
    }
    let mut greater = zero_mask();
    let mut equal = *within;
    for (i, plane) in slice.planes().iter().enumerate().rev() {
        if t >> i & 1 == 0 {
            for w in 0..BLOCK_WORDS {
                greater[w] |= equal[w] & plane[w];
                equal[w] &= !plane[w];
            }
        } else {
            for w in 0..BLOCK_WORDS {
                equal[w] &= plane[w];
            }
        }
    }
    for w in 0..BLOCK_WORDS {
        greater[w] |= equal[w];
    }
    greater
}

/// Ordinal offsets of the set bits of a mask, ascending.
pub fn offsets(m: &BlockMask, out: &mut Vec<u32>) {
    out.clear();
    for (w, word) in m.iter().enumerate() {
        let mut x = *word;
        while x != 0 {
            out.push((w as u32) * 64 + x.trailing_zeros());
            x &= x - 1;
        }
    }
}

#[cfg(test)]
mod tiled_tests {
    use super::{BlockMask, Csa, Slice, tiled_into, zero_mask};

    fn next(seed: &mut u64) -> u64 {
        *seed = seed.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = *seed;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    #[test]
    fn tiled_matches_plane_wide_csa_and_ripple_at_every_word() {
        for dimensions in [0usize, 1, 2, 15, 16, 17, 31, 32, 33, 133, 256] {
            for (shift, z_planes) in [(0usize, 0usize), (1, 0), (1, 9)] {
                let max = (dimensions << shift) + (1usize << z_planes) - 1;
                let levels = super::levels_for(max.max(1) as u32);
                let mut seed = 0x517e_d1e5_u64 ^ dimensions as u64 ^ (z_planes as u64) << 16;
                let lanes: Vec<BlockMask> = (0..dimensions + z_planes)
                    .map(|lane| {
                        let mut mask = zero_mask();
                        // Whole absent lanes, sparse array-like lanes, runs and
                        // dense bitmaps all participate in the same arithmetic.
                        if lane % 7 == 0 {
                            return mask;
                        }
                        for (word, value) in mask.iter_mut().enumerate() {
                            let raw = next(&mut seed);
                            *value = match lane % 4 {
                                0 => raw,
                                1 => raw & raw.rotate_left(17) & raw.rotate_left(37),
                                2 => {
                                    if word % 3 == 0 {
                                        u64::MAX
                                    } else {
                                        0
                                    }
                                }
                                _ => raw & 1,
                            };
                        }
                        mask
                    })
                    .collect();
                let mut tiled = Slice::new(levels);
                tiled_into(&lanes, dimensions, z_planes, shift, &mut tiled);
                let mut csa = Csa::new(levels);
                let mut ripple = Slice::new(levels);
                for lane in &lanes[..dimensions] {
                    csa.push_at(lane, shift);
                    ripple.add_plane_at(lane, shift);
                }
                let mut wide = Slice::new(levels);
                csa.finish_into(&mut wide);
                for (j, lane) in lanes[dimensions..].iter().enumerate() {
                    wide.add_plane_at(lane, j);
                    ripple.add_plane_at(lane, j);
                }
                assert_eq!(
                    tiled.planes(),
                    wide.planes(),
                    "CSA: n={dimensions} z={z_planes} shift={shift}"
                );
                assert_eq!(
                    tiled.planes(),
                    ripple.planes(),
                    "ripple: n={dimensions} z={z_planes} shift={shift}"
                );
            }
        }
    }
}
