//! Carry-save against ripple-carry against composed set operations.
//!
//! # The frame, stated because it is the thing that keeps breaking
//!
//! Every number here is **word operations over one 65 536-ordinal block, in
//! haiiie's own plane buffers**. It is not a measurement of yesnodb container
//! operations, and it must not be quoted as one: at container granularity the
//! word-operation model is the wrong one outright, because allocation,
//! cardinality bookkeeping and kind dispatch dominate. The `composed` arm below
//! exists to show that difference rather than to be a fair third contender.
//!
//! The derived expectation, so a measurement can contradict it rather than be
//! read as confirming whatever it says: ripple costs `2L` operations per addend
//! and carry-save costs `5` independent of `L`, for a ratio of `2L/5` -- 3.2x at
//! `L = 8`, 4.0x at `L = 10`. An earlier draft of the upstream prescription said
//! `3L` and quoted 4.8x and 6.0x; that was an asymmetric count corrected before
//! this benchmark existed, and the corrected pair is what this should land near.

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use haiiie_core::slice::{BLOCK_WORDS, BlockMask, Csa, Slice, levels_for, zero_mask};
use std::hint::black_box;

fn masks(n: usize, seed: u64) -> Vec<BlockMask> {
    let mut state = seed;
    let mut next = || {
        state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    };
    (0..n)
        .map(|_| {
            let mut m = zero_mask();
            for w in m.iter_mut().take(BLOCK_WORDS) {
                *w = next();
            }
            m
        })
        .collect()
}

fn bench(c: &mut Criterion) {
    let mut g = c.benchmark_group("accumulate-one-block");
    // Query widths spanning the range a binary code search actually produces:
    // 32 is a sparse learned-retrieval query, 512 a dense sign-quantized one.
    for &n in &[32usize, 128, 512] {
        let addends = masks(n, 0x5EED ^ n as u64);
        let levels = levels_for(u32::try_from(n).unwrap());

        g.bench_with_input(BenchmarkId::new("ripple", n), &addends, |b, a| {
            b.iter(|| {
                let mut s = Slice::new(levels);
                for m in a {
                    s.add_plane_at(m, 0);
                }
                black_box(s.value_at(0))
            });
        });

        g.bench_with_input(BenchmarkId::new("carry-save", n), &addends, |b, a| {
            let mut csa = Csa::new(levels);
            let mut out = Slice::new(levels);
            b.iter(|| {
                for m in a {
                    csa.push_at(m, 0);
                }
                csa.finish_into(&mut out);
                black_box(out.value_at(0))
            });
        });

        // Ripple again, but level-major with a reused carry buffer instead of
        // word-major with a per-word early exit. Same arithmetic, same result,
        // different loop order -- included because the first run of this
        // benchmark had `composed-allocating` *beating* word-major ripple,
        // which allocation cost cannot explain and loop order can.
        g.bench_with_input(
            BenchmarkId::new("ripple-level-major", n),
            &addends,
            |b, a| {
                let mut planes = vec![zero_mask(); levels];
                let mut carry = zero_mask();
                b.iter(|| {
                    for p in &mut planes {
                        p.fill(0);
                    }
                    for m in a {
                        carry.copy_from_slice(m);
                        for plane in &mut planes {
                            for w in 0..BLOCK_WORDS {
                                let t = plane[w] & carry[w];
                                plane[w] ^= carry[w];
                                carry[w] = t;
                            }
                        }
                    }
                    black_box(planes[0][0])
                });
            },
        );

        // What the same accumulation costs when each step allocates its result,
        // which is what routing planes through a container-level set operator
        // would do. Not a contender: the point is the gap between "five word
        // operations in a reused buffer" and "one allocation per step".
        g.bench_with_input(
            BenchmarkId::new("composed-allocating", n),
            &addends,
            |b, a| {
                b.iter(|| {
                    let mut planes: Vec<Vec<u64>> = vec![vec![0u64; BLOCK_WORDS]; levels];
                    for m in a {
                        let mut carry = m.to_vec();
                        for plane in &mut planes {
                            let t: Vec<u64> =
                                plane.iter().zip(&carry).map(|(p, c)| p & c).collect();
                            let s: Vec<u64> =
                                plane.iter().zip(&carry).map(|(p, c)| p ^ c).collect();
                            *plane = s;
                            carry = t;
                        }
                    }
                    black_box(planes[0][0])
                });
            },
        );
    }
    g.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
