//! The carry-save kernel against the ripple-carry oracle.
//!
//! These are two implementations of one function where only the slow one is
//! obviously correct, so every property here is agreement rather than a claim
//! about either in isolation. The ripple version is never deleted for being
//! slower; that is the whole arrangement.

use haiiie_core::slice::{self, BLOCK_WORDS, BlockMask, Csa, Slice, zero_mask};

/// Deterministic SplitMix64, so a failure is reproducible from its seed alone.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    /// A mask at roughly `num/den` density -- both extremes matter, since a
    /// carry that is always empty and one that is never empty exercise
    /// different halves of the promotion logic.
    fn mask(&mut self, num: u32, den: u32) -> BlockMask {
        let mut m = zero_mask();
        for w in m.iter_mut().take(BLOCK_WORDS) {
            let mut acc = 0u64;
            for b in 0..64 {
                if self.next() % u64::from(den) < u64::from(num) {
                    acc |= 1 << b;
                }
            }
            *w = acc;
        }
        m
    }
}

fn ripple(addends: &[BlockMask], levels: usize, shift: usize) -> Slice {
    let mut s = Slice::new(levels);
    for a in addends {
        s.add_plane_at(a, shift);
    }
    s
}

fn carry_save(addends: &[BlockMask], levels: usize, shift: usize) -> Slice {
    let mut c = Csa::new(levels);
    for a in addends {
        c.push_at(a, shift);
    }
    c.finish()
}

fn assert_same(a: &Slice, b: &Slice, what: &str) {
    assert_eq!(a.levels(), b.levels(), "{what}: level count");
    for off in 0..(BLOCK_WORDS as u32) * 64 {
        assert_eq!(a.value_at(off), b.value_at(off), "{what}: offset {off}");
    }
}

#[test]
fn carry_save_agrees_with_ripple_at_every_addend_count() {
    // Boundaries first: the level count is `ceil(log2(n+1))`, so `n` at a power
    // of two and one below it is where an off-by-one in promotion hides.
    let mut counts: Vec<usize> = (0..=17).collect();
    counts.extend([31, 32, 33, 63, 64, 65, 127, 128]);
    for n in counts {
        let levels = slice::levels_for(u32::try_from(n).unwrap().max(1));
        for (num, den) in [(1u32, 2u32), (1, 64), (63, 64), (1, 1)] {
            let mut rng = Rng(0xA11CE ^ (n as u64) << 8 ^ u64::from(num));
            let addends: Vec<BlockMask> = (0..n).map(|_| rng.mask(num, den)).collect();
            assert_same(
                &ripple(&addends, levels, 0),
                &carry_save(&addends, levels, 0),
                &format!("n={n} density={num}/{den}"),
            );
        }
    }
}

#[test]
fn carry_save_agrees_with_ripple_at_every_shift() {
    for shift in 0..4usize {
        let n = 9;
        let levels = slice::levels_for(1 << (shift + 4));
        let mut rng = Rng(0xBEEF ^ shift as u64);
        let addends: Vec<BlockMask> = (0..n).map(|_| rng.mask(1, 2)).collect();
        assert_same(
            &ripple(&addends, levels, shift),
            &carry_save(&addends, levels, shift),
            &format!("shift={shift}"),
        );
    }
}

#[test]
fn the_accumulated_value_is_the_count_of_addends_covering_each_ordinal() {
    // Agreement between two implementations is not enough on its own: both
    // could be wrong the same way. This pins the value against a scalar count.
    let n = 21;
    let levels = slice::levels_for(n);
    let mut rng = Rng(0xC0FFEE);
    let addends: Vec<BlockMask> = (0..n as usize).map(|_| rng.mask(1, 3)).collect();
    let got = carry_save(&addends, levels, 0);
    for off in [0u32, 1, 63, 64, 65, 1000, 65_535] {
        let (w, b) = (off as usize >> 6, off & 63);
        let want = addends.iter().filter(|m| m[w] >> b & 1 == 1).count() as u32;
        assert_eq!(got.value_at(off), want, "offset {off}");
    }
}

#[test]
fn an_empty_addend_set_is_all_zero_and_a_full_one_saturates_correctly() {
    let s = carry_save(&[], 4, 0);
    assert_eq!(s.value_at(0), 0);

    // Every addend covering every ordinal: the value must be exactly `n`, which
    // is the case that overflows a slice sized one level too small.
    for n in [1usize, 3, 7, 8, 15, 16] {
        let full = [u64::MAX; BLOCK_WORDS];
        let addends = vec![full; n];
        let levels = slice::levels_for(u32::try_from(n).unwrap());
        let s = carry_save(&addends, levels, 0);
        assert_eq!(s.value_at(0), n as u32, "n={n}");
        assert_eq!(s.value_at(65_535), n as u32, "n={n} at the last ordinal");
    }
}

#[test]
fn add_slice_at_shifts_a_whole_slice() {
    let mut a = Slice::new(3);
    let mut m = zero_mask();
    m[0] = 0b101;
    a.add_plane_at(&m, 0);
    a.add_plane_at(&m, 1); // value 3 at offsets 0 and 2

    let mut s = Slice::new(6);
    s.add_slice_at(&a, 1); // 2 * 3 == 6
    assert_eq!(s.value_at(0), 6);
    assert_eq!(s.value_at(1), 0);
    assert_eq!(s.value_at(2), 6);

    s.add_slice_at(&a, 0); // + 3 == 9
    assert_eq!(s.value_at(0), 9);
}

#[test]
fn ge_selects_exactly_the_values_at_or_above_the_threshold() {
    // Exhaustive over a small value set against a scalar filter. The comparator
    // has a branch per plane on the threshold's bits, so every threshold in
    // range is a distinct path and sampling would leave some unrun.
    let levels = 4;
    let mut s = Slice::new(levels);
    let mut within = zero_mask();
    let n = 40usize;
    let vals: Vec<u32> = (0..n).map(|i| ((i * 5) % 16) as u32).collect();
    for (i, v) in vals.iter().enumerate() {
        within[i >> 6] |= 1u64 << (i & 63);
        for j in 0..levels {
            if v >> j & 1 == 1 {
                let mut m = zero_mask();
                m[i >> 6] |= 1u64 << (i & 63);
                s.add_plane_at(&m, j);
            }
        }
    }
    for t in 0..=16u32 {
        let got = slice::ge(&s, &within, t);
        let mut offs = Vec::new();
        slice::offsets(&got, &mut offs);
        let want: Vec<u32> = (0..n as u32).filter(|&i| vals[i as usize] >= t).collect();
        assert_eq!(offs, want, "threshold {t}");
    }
}

#[test]
fn ge_never_selects_outside_its_candidate_set() {
    // A refinement pass intersects with the live and filtered set; if `ge`
    // returned anything outside `within`, a deleted or excluded document could
    // re-enter the result.
    let mut s = Slice::new(3);
    let mut all = zero_mask();
    all[0] = u64::MAX;
    s.add_plane_at(&all, 0); // every ordinal in word 0 has value 1
    let mut within = zero_mask();
    within[0] = 0b1010;
    let got = slice::ge(&s, &within, 0);
    assert_eq!(
        got[0], 0b1010,
        "selected ordinals outside the candidate set"
    );
    assert!(got[1..].iter().all(|w| *w == 0));
}
