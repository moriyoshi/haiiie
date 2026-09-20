//! M2's gate: the inverted path agrees with the forward paths and the oracle.
//!
//! `Gather`, `DenseScan` and the three inverted kernels are forced against
//! the same oracle. Ripple is the slow, obvious integer implementation;
//! plane-wide CSA and the default tiled kernel must agree with it and with
//! both forward paths, including full scores and tie order.

use haiiie_core::{
    CodeRef, DocId, Filter, Index, Kernel, Metric, Path, PathHint, SetStore, Slice, YesnoStore,
    slice,
};
use haiiie_testkit::{Corpus, MemStore, Shape, oracle};

const SHAPES: [Shape; 5] = [
    Shape::Balanced,
    Shape::Sparse,
    Shape::Clustered,
    Shape::Degenerate,
    Shape::OrdinalRuns,
];
/// All four. Dot and Hamming rank by one bit-sliced integer; Jaccard and
/// cosine are ratios and go through the monotone-bound refinement pass, which
/// is a different code path in the inverted scan and must reach the same answer.
const METRICS: [Metric; 4] = [
    Metric::Dot,
    Metric::Hamming,
    Metric::Jaccard,
    Metric::Cosine,
];

fn ingest<S: SetStore>(store: S, c: &Corpus) -> Index<S> {
    let idx = Index::create(store, 3, c.dims).expect("create");
    let mut w = idx.writer();
    for i in 0..c.len() {
        w.put(DocId(i as u64), c.code(i)).expect("put");
    }
    w.commit().expect("commit");
    idx
}

#[test]
fn every_path_agrees_with_the_oracle_on_every_shape() {
    for shape in SHAPES {
        let c = Corpus::generate(101, 128, 40, shape);
        let idx = ingest(MemStore::new(), &c);
        for metric in METRICS {
            for k in [1usize, 3, 40] {
                let want = oracle::top_k(&c, c.code(0), metric, k, None, None);
                // Four implementations of one function; three of them are
                // oracles for whichever one is under suspicion.
                for (hint, kernel) in [
                    (PathHint::Gather, Kernel::CarrySave),
                    (PathHint::DenseScan, Kernel::CarrySave),
                    (PathHint::Inverted, Kernel::Ripple),
                    (PathHint::Inverted, Kernel::CarrySave),
                    (PathHint::Inverted, Kernel::Tiled),
                ] {
                    let got = idx
                        .search()
                        .code(c.code(0))
                        .metric(metric)
                        .k(k)
                        .path(hint)
                        .kernel(kernel)
                        .execute()
                        .expect("search");
                    assert_eq!(
                        got.hits, want,
                        "{shape:?}/{metric:?}/k={k}/{hint:?}/{kernel:?}"
                    );
                }
            }
        }
    }
}

/// A large Hamming tie class contains different `(intersection, weight)`
/// pairs. The inverted path must retain the smallest IDs and return every
/// pair exactly, including after a persisted lane visit.
#[test]
fn hamming_tie_trimming_preserves_ids_and_fields_after_reopen() {
    let query = 0xFFFF_FFFF_0000_0000u64;
    let codes = (0..96)
        .map(|i| {
            let code = match i % 3 {
                0 => query ^ 0x0003_0000_0000_0000, // remove two query bits
                1 => query ^ 0x0001_0000_0000_0001, // swap one bit
                _ => query ^ 0x0000_0000_0000_0003, // add two other bits
            };
            vec![code]
        })
        .collect();
    let corpus = Corpus { dims: 64, codes };
    let dir = tempfile::tempdir().expect("temporary store");
    let idx = ingest(YesnoStore::open(dir.path()).expect("store"), &corpus);
    idx.store().flush().expect("flush");
    drop(idx);
    let idx =
        Index::open(YesnoStore::open(dir.path()).expect("reopen store"), 3).expect("reopen index");
    let query = CodeRef::Dense(&[query]);
    let want = oracle::top_k(&corpus, query, Metric::Hamming, 5, None, None);
    assert_eq!(
        want.iter().map(|h| h.id).collect::<Vec<_>>(),
        (0..5).map(DocId).collect::<Vec<_>>()
    );
    assert!(want.windows(2).any(|hits| hits[0].weight != hits[1].weight));
    for kernel in [Kernel::Tiled, Kernel::CarrySave, Kernel::Ripple] {
        let got = idx
            .search()
            .code(query)
            .metric(Metric::Hamming)
            .k(5)
            .path(PathHint::Inverted)
            .kernel(kernel)
            .execute()
            .expect("search");
        assert_eq!(got.hits, want, "{kernel:?}");
        assert_eq!(got.scored, 5, "{kernel:?} scored more than block top-k");
    }
}

/// The small-k heap and large-k selection meet at 128. Ties and a selective
/// filter exercise the same total order on both sides of that boundary.
#[test]
fn forward_top_k_matches_the_oracle_across_the_heap_boundary() {
    let c = Corpus::generate(211, 64, 600, Shape::Degenerate);
    let idx = ingest(MemStore::new(), &c);
    let mut w = idx.writer();
    for i in (0..c.len()).step_by(3) {
        w.attr(DocId(i as u64), 12).expect("attr");
    }
    w.commit().expect("attrs");
    let admitted: std::collections::BTreeSet<usize> = (0..c.len()).step_by(3).collect();
    for metric in METRICS {
        for k in [0usize, 1, 10, 128, 129, 512] {
            let want = oracle::top_k(&c, c.code(1), metric, k, None, Some(&admitted));
            for hint in [PathHint::Gather, PathHint::DenseScan] {
                let got = idx
                    .search()
                    .code(c.code(1))
                    .metric(metric)
                    .k(k)
                    .filter(Filter::Term(12))
                    .path(hint)
                    .execute()
                    .expect("search");
                assert_eq!(got.hits, want, "{metric:?}/k={k}/{hint:?}");
                assert_eq!(
                    got.scored,
                    admitted.len() as u64,
                    "scored at k={k}/{hint:?}"
                );
            }
        }
    }
}

/// The two accumulators must agree through the whole search, not merely in the
/// unit test: a promotion bug that only shows at a real query width, or only
/// once `z` planes are folded in afterwards, would pass `tests/csa.rs`.
#[test]
fn the_two_accumulators_agree_through_a_full_search() {
    for shape in SHAPES {
        let c = Corpus::generate(127, 256, 36, shape);
        let idx = ingest(MemStore::new(), &c);
        for metric in METRICS {
            let r = idx
                .search()
                .code(c.code(4))
                .metric(metric)
                .k(usize::MAX)
                .path(PathHint::Inverted)
                .kernel(Kernel::Ripple)
                .execute()
                .expect("ripple");
            let s = idx
                .search()
                .code(c.code(4))
                .metric(metric)
                .k(usize::MAX)
                .path(PathHint::Inverted)
                .kernel(Kernel::CarrySave)
                .execute()
                .expect("csa");
            let t = idx
                .search()
                .code(c.code(4))
                .metric(metric)
                .k(usize::MAX)
                .path(PathHint::Inverted)
                .kernel(Kernel::Tiled)
                .execute()
                .expect("tiled");
            assert_eq!(r.hits, s.hits, "{shape:?}/{metric:?}");
            assert_eq!(r.hits, t.hits, "tiled {shape:?}/{metric:?}");
            assert_eq!(r.scored, s.scored, "{shape:?}/{metric:?}: different work");
            assert_eq!(
                r.scored, t.scored,
                "tiled {shape:?}/{metric:?}: different work"
            );
        }
    }
}

/// The inverted path must report the same `(a, w)` pair the forward path does,
/// not merely the same ranking. It reconstructs both from the bit-sliced value
/// rather than reading the code, so a wrong reconstruction would still rank
/// correctly for Hamming and be wrong in the returned `Hit`.
#[test]
fn the_inverted_path_reconstructs_the_same_counts() {
    for shape in SHAPES {
        let c = Corpus::generate(103, 192, 30, shape);
        let idx = ingest(MemStore::new(), &c);
        for metric in METRICS {
            let f = idx
                .search()
                .code(c.code(2))
                .metric(metric)
                .k(usize::MAX)
                .path(PathHint::Gather)
                .execute()
                .expect("gather");
            let i = idx
                .search()
                .code(c.code(2))
                .metric(metric)
                .k(usize::MAX)
                .path(PathHint::Inverted)
                .execute()
                .expect("inverted");
            assert_eq!(f.hits, i.hits, "{shape:?}/{metric:?}");
            assert_eq!(i.path, Path::Inverted);
        }
    }
}

#[test]
fn the_inverted_path_composes_with_a_filter_and_skips_empty_blocks() {
    let c = Corpus::generate(107, 128, 48, Shape::Balanced);
    let idx = ingest(MemStore::new(), &c);
    {
        let mut w = idx.writer();
        for i in (0..c.len()).step_by(4) {
            w.attr(DocId(i as u64), 9).expect("attr");
        }
        w.commit().expect("attrs");
    }
    let admitted: std::collections::BTreeSet<usize> = (0..c.len()).step_by(4).collect();
    for metric in METRICS {
        let want = oracle::top_k(&c, c.code(1), metric, 4, None, Some(&admitted));
        let got = idx
            .search()
            .code(c.code(1))
            .metric(metric)
            .k(4)
            .filter(Filter::Term(9))
            .path(PathHint::Inverted)
            .execute()
            .expect("search");
        assert_eq!(got.hits, want, "{metric:?}");
    }
}

/// Blocked on an upstream crash, **not** disabled for being inconvenient.
///
/// `yesno-core/src/index/node.rs:304` panics with a subtraction overflow when a
/// leaf's key-suffix width is 10 or 12: `LeafRef::search` takes its safe
/// whole-key path only at width 14, and `suffix_u64` is valid only up to 8.
/// haiiie's key layout puts the differing bit at 20, so a chunk key needs a
/// 9-byte suffix, which rounds to the first broken width. Reported upstream
/// with a two-key reproduction at `.agents-workspace/tmp/ksuf-repro`.
///
/// Kept rather than deleted, and the key layout is deliberately **not** bent to
/// dodge it: the schema should be shaped by what haiiie needs, not by a bug
/// upstream is fixing. Re-enable when they report it closed.
#[test]
fn the_inverted_path_survives_a_reopen() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("db");
    let c = Corpus::generate(109, 256, 24, Shape::Clustered);
    let before = {
        let idx = ingest(YesnoStore::open(&path).expect("open"), &c);
        idx.store().flush().expect("checkpoint");
        idx.search()
            .code(c.code(5))
            .metric(Metric::Hamming)
            .k(8)
            .path(PathHint::Inverted)
            .execute()
            .expect("search")
            .hits
    };
    let idx = Index::open(YesnoStore::open(&path).expect("reopen"), 3).expect("index");
    let after = idx
        .search()
        .code(c.code(5))
        .metric(Metric::Hamming)
        .k(8)
        .path(PathHint::Inverted)
        .execute()
        .expect("search")
        .hits;
    assert_eq!(before, after);
}

/// A replace must clear the old posting-list and weight-plane membership too,
/// not only the forward row. If it did not, the inverted path would score a
/// document by the union of its old and new codes while the forward path
/// scored it correctly -- which is precisely what the cross-path comparison
/// above would catch, so this pins it directly.
#[test]
fn a_replace_clears_every_view() {
    let c = Corpus::generate(113, 64, 6, Shape::Balanced);
    let idx = ingest(MemStore::new(), &c);
    let empty = [0u64; 1];
    let mut w = idx.writer();
    w.put(DocId(2), CodeRef::Dense(&empty)).expect("put");
    w.commit().expect("commit");

    for hint in [PathHint::Gather, PathHint::Inverted] {
        let hits = idx
            .search()
            .code(c.code(2))
            .metric(Metric::Dot)
            .k(usize::MAX)
            .path(hint)
            .execute()
            .expect("search");
        let h = hits
            .hits
            .iter()
            .find(|h| h.id == DocId(2))
            .expect("present");
        assert_eq!((h.inter, h.weight), (0, 0), "{hint:?} saw a stale code");
    }
}

// ---- the slice arithmetic itself -------------------------------------------

#[test]
fn ripple_carry_adds_what_it_claims() {
    let mut s = Slice::new(slice::levels_for(7));
    let mut a = slice::zero_mask();
    a[0] = 0b1011;
    s.add_plane_at(&a, 0);
    s.add_plane_at(&a, 0);
    let mut b = slice::zero_mask();
    b[0] = 0b0010;
    s.add_plane_at(&b, 0);
    assert_eq!(s.value_at(0), 2, "bit 0 was in `a` twice");
    assert_eq!(s.value_at(1), 3, "bit 1 was in `a` twice and `b` once");
    assert_eq!(s.value_at(2), 0);
    assert_eq!(s.value_at(3), 2);
}

#[test]
fn a_shifted_addend_is_a_weighted_one() {
    let mut s = Slice::new(4);
    let mut a = slice::zero_mask();
    a[0] = 1;
    s.add_plane_at(&a, 2);
    assert_eq!(s.value_at(0), 4);
    s.add_plane_at(&a, 0);
    assert_eq!(s.value_at(0), 5);
}

#[test]
fn the_descent_finds_the_true_top_k() {
    // Exhaustive over small value sets, against a sort. The descent's two
    // subtle points -- the strict comparison and the early exit -- both show up
    // only at exact boundaries, so sampling would miss them.
    for n in 1..24usize {
        let mut s = Slice::new(4);
        let mut within = slice::zero_mask();
        let vals: Vec<u32> = (0..n).map(|i| ((i * 7) % 13) as u32 % 8).collect();
        for (i, v) in vals.iter().enumerate() {
            within[i >> 6] |= 1u64 << (i & 63);
            for j in 0..4 {
                if v >> j & 1 == 1 {
                    let mut m = slice::zero_mask();
                    m[i >> 6] |= 1u64 << (i & 63);
                    s.add_plane_at(&m, j);
                }
            }
        }
        for k in 0..=n {
            let got = slice::top_k(&s, &within, k);
            let mut picked: Vec<u32> = Vec::new();
            let mut offs = Vec::new();
            slice::offsets(&got.confirmed, &mut offs);
            picked.extend(offs.iter().map(|&o| s.value_at(o)));
            let conf = picked.len();
            slice::offsets(&got.tied, &mut offs);
            let tied: Vec<u32> = offs.iter().map(|&o| s.value_at(o)).collect();

            assert!(conf <= k, "confirmed {conf} exceeds k = {k}");
            assert!(
                tied.windows(2).all(|w| w[0] == w[1]),
                "tie class is not tied"
            );

            let mut sorted = vals.clone();
            sorted.sort_unstable_by(|a, b| b.cmp(a));
            picked.sort_unstable_by(|a, b| b.cmp(a));
            assert_eq!(picked, sorted[..conf], "n={n} k={k}: wrong confirmed set");
            assert!(conf + tied.len() >= k.min(n), "n={n} k={k}: cannot fill k");
        }
    }
}

/// The refinement pass must actually narrow, or it is inert and untested.
///
/// # Why this assertion exists
///
/// `refine` returns the whole candidate set when it cannot form a threshold --
/// correct, and indistinguishable in every other test from a refinement that
/// works. If a future change made that the only branch taken, every four-metric
/// suite above would still pass while `slice::ge` went unexercised. So this
/// asserts the precondition that makes those suites load-bearing, in the same
/// shape as `key_layout.rs`: the fixture must still reach its subject.
///
/// It deliberately asserts *that* it narrows, not *how much*. The bound for a
/// ratio metric is `a/m`, which is loose -- measured here at 328 and 391
/// documents scored out of 400 at `k = 10`, against 13 for the linear metrics.
/// Pinning those numbers would make this a performance test that fails on
/// unrelated corpus changes; the exactness is what matters at M4 and the
/// tightening is M5's, when per-block weight ranges arrive.
#[test]
fn the_ratio_metrics_actually_use_the_refinement_pass() {
    let c = Corpus::generate(211, 128, 400, Shape::Balanced);
    let idx = ingest(MemStore::new(), &c);
    let live = idx.len().expect("len");

    for metric in [Metric::Jaccard, Metric::Cosine] {
        let got = idx
            .search()
            .code(c.code(0))
            .metric(metric)
            .k(10)
            .path(PathHint::Inverted)
            .execute()
            .expect("search");
        assert!(
            got.scored < live,
            "{metric:?} scored every one of {live} documents; the monotone bound \
             is not narrowing and `slice::ge` is not being exercised"
        );
        assert!(got.scored >= 10, "{metric:?} scored fewer than k documents");
    }

    // And the linear metrics must narrow far harder, since their descent is
    // exact rather than provisional. If these ever converge, the ranking key
    // for Dot or Hamming has stopped being exact.
    for metric in [Metric::Dot, Metric::Hamming] {
        let got = idx
            .search()
            .code(c.code(0))
            .metric(metric)
            .k(10)
            .path(PathHint::Inverted)
            .execute()
            .expect("search");
        assert!(
            got.scored < live / 4,
            "{metric:?} scored {} of {live}; a linear metric should need only \
             the descent's pick",
            got.scored
        );
    }
}

/// `Auto` must choose per block and still return the exact answer.
///
/// The planner is the only place a path is picked without a test forcing it, so
/// this is the one check that the chosen path is not merely fast but right. It
/// runs both extremes of selectivity, because the two arms of the decision are
/// different code.
#[test]
fn the_planner_returns_the_same_answer_as_either_forced_path() {
    let c = Corpus::generate(223, 128, 3_000, Shape::Balanced);
    let idx = ingest(MemStore::new(), &c);
    {
        let mut w = idx.writer();
        // Two filters straddling the crossover: 1 in 2 and 1 in 1000.
        for i in 0..c.len() {
            if i % 2 == 0 {
                w.attr(DocId(i as u64), 1).expect("attr");
            }
            if i % 1_000 == 0 {
                w.attr(DocId(i as u64), 2).expect("attr");
            }
        }
        w.commit().expect("attrs");
    }

    for term in [1u32, 2] {
        for metric in METRICS {
            let forced: Vec<_> = [PathHint::Inverted, PathHint::Gather]
                .iter()
                .map(|&h| {
                    idx.search()
                        .code(c.code(0))
                        .metric(metric)
                        .k(7)
                        .filter(Filter::Term(term))
                        .path(h)
                        .execute()
                        .expect("forced")
                        .hits
                })
                .collect();
            assert_eq!(forced[0], forced[1], "forced paths disagree: term {term}");

            let auto = idx
                .search()
                .code(c.code(0))
                .metric(metric)
                .k(7)
                .filter(Filter::Term(term))
                .path(PathHint::Auto)
                .execute()
                .expect("auto");
            assert_eq!(
                auto.hits, forced[0],
                "Auto disagrees: term {term}/{metric:?}"
            );
        }
    }
}
