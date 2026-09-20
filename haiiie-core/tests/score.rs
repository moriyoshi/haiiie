//! Score ordering is exact, total, and independent of floating point.

use haiiie_core::{Metric, Score};

#[test]
fn ratios_compare_without_rounding() {
    // 1/3 vs 33333333/100000000: equal as f32, distinct exactly.
    let a = Score::ratio(1, 3);
    let b = Score::ratio(33_333_333, 100_000_000);
    assert!(a > b, "exact comparison must separate what f32 would merge");
    assert_eq!(a.cmp(&a), std::cmp::Ordering::Equal);
}

#[test]
fn a_zero_denominator_is_an_honest_zero() {
    assert_eq!(Score::ratio(5, 0), Score::Ratio { num: 0, den: 1 });
    assert_eq!(Score::ratio_sq(5, 0), Score::RatioSq { num: 0, den: 1 });
}

#[test]
fn cosine_ordering_does_not_depend_on_the_query_weight() {
    // cos = a/sqrt(m*w); comparing two documents, m cancels. If it ever stops
    // cancelling, the refinement threshold in `min_intersection` is wrong too.
    for m in [1u32, 7, 4096] {
        let x = Metric::Cosine.score(3, 9, m);
        let y = Metric::Cosine.score(2, 3, m);
        assert!(y > x, "m = {m} changed the cosine ordering");
    }
}

#[test]
fn hamming_ranks_as_the_negated_distance() {
    // -H must be order-isomorphic to H reversed, for every (a, w) pair.
    let m = 64;
    for w in 0..=64u32 {
        for a in 0..=w.min(m) {
            let d = Metric::hamming_distance(a, w, m);
            assert_eq!(Metric::Hamming.score(a, w, m), Score::Int(-i64::from(d)));
        }
    }
}

#[test]
fn the_bound_is_an_upper_bound_for_every_metric() {
    // The correctness of every future refinement pass rests on this.
    let m = 40;
    for metric in [
        Metric::Dot,
        Metric::Hamming,
        Metric::Jaccard,
        Metric::Cosine,
    ] {
        for w in 1..=64u32 {
            for a in 0..=w.min(m) {
                let exact = metric.score(a, w, m);
                let bound = metric.bound(a, m);
                assert!(
                    bound >= exact,
                    "{metric:?}: bound({a}) = {bound:?} < exact({a},{w}) = {exact:?}"
                );
            }
        }
    }
}

#[test]
fn the_bound_is_monotone_in_the_intersection() {
    let m = 40;
    for metric in [
        Metric::Dot,
        Metric::Hamming,
        Metric::Jaccard,
        Metric::Cosine,
    ] {
        for a in 1..=m {
            assert!(
                metric.bound(a, m) >= metric.bound(a - 1, m),
                "{metric:?}: bound is not monotone at a = {a}"
            );
        }
    }
}

#[test]
fn min_intersection_never_excludes_a_document_that_could_reach_tau() {
    // The refinement pass drops everything below `min_intersection(tau)`. If
    // that threshold is ever too high, the dropped document is a *missing
    // result*, which no crash and no cardinality check would reveal.
    let m = 32;
    for metric in [
        Metric::Dot,
        Metric::Hamming,
        Metric::Jaccard,
        Metric::Cosine,
    ] {
        for w in 1..=48u32 {
            for a in 0..=w.min(m) {
                let tau = metric.score(a, w, m);
                let need = metric.min_intersection(tau, m);
                assert!(
                    need <= a,
                    "{metric:?}: tau from a = {a} demands {need}, excluding its own source"
                );
            }
        }
    }
}

// The `w_min` half of the bound had none of the three properties above, while
// its siblings had all of them.
//
// `Metric::bound_with` and `Metric::min_intersection_with` are what per-block
// weight statistics buy -- a measured 3.6x fewer documents given an exact score
// on the ratio metrics -- and they are reached from the scan through
// `min_intersection_with`, so nothing named them in a test. Their failure is the
// one their own doc comment warns about: a threshold that is too high drops a
// document silently, with no crash, no decode error and no cardinality
// discrepancy. End-to-end suites cover it on the corpora they happen to use;
// these cover the domain.

#[test]
fn a_zero_floor_reduces_the_tightened_bound_to_the_loose_one() {
    // The doc comment claims this exactly, and the scan relies on it: a block
    // with no statistics passes `w_min = 0` and must behave as before.
    let m = 40;
    for metric in [
        Metric::Dot,
        Metric::Hamming,
        Metric::Jaccard,
        Metric::Cosine,
    ] {
        for a in 0..=m {
            assert_eq!(
                metric.bound_with(a, m, 0),
                metric.bound(a, m),
                "{metric:?}: w_min = 0 must reduce to the loose bound at a = {a}"
            );
        }
    }
}

#[test]
fn the_tightened_bound_is_still_an_upper_bound_for_every_metric() {
    let m = 32;
    for metric in [
        Metric::Dot,
        Metric::Hamming,
        Metric::Jaccard,
        Metric::Cosine,
    ] {
        for w_min in 0..=48u32 {
            // Only weights at or above the block's floor can occur in it.
            for w in w_min.max(1)..=48u32 {
                for a in 0..=w.min(m) {
                    let exact = metric.score(a, w, m);
                    let bound = metric.bound_with(a, m, w_min);
                    assert!(
                        bound >= exact,
                        "{metric:?}: bound_with(a={a}, m={m}, w_min={w_min}) = {bound:?} \
                         is below the exact score {exact:?} at w = {w}"
                    );
                }
            }
        }
    }
}

#[test]
fn the_tightened_bound_never_exceeds_the_loose_one() {
    // A "tightening" that loosened would be harmless for correctness and would
    // silently cost the entire measured win, which no other test would notice.
    let m = 32;
    for metric in [
        Metric::Dot,
        Metric::Hamming,
        Metric::Jaccard,
        Metric::Cosine,
    ] {
        for w_min in 0..=48u32 {
            for a in 0..=m {
                assert!(
                    metric.bound_with(a, m, w_min) <= metric.bound(a, m),
                    "{metric:?}: bound_with at a = {a}, w_min = {w_min} is looser than bound"
                );
            }
        }
    }
}

#[test]
fn the_tightened_bound_is_monotone_in_the_intersection() {
    // `min_intersection_with` inverts this by a linear scan, which is only a
    // valid inverse if the function is monotone.
    let m = 40;
    for metric in [
        Metric::Dot,
        Metric::Hamming,
        Metric::Jaccard,
        Metric::Cosine,
    ] {
        for w_min in 0..=40u32 {
            for a in 1..=m {
                assert!(
                    metric.bound_with(a, m, w_min) >= metric.bound_with(a - 1, m, w_min),
                    "{metric:?}: not monotone at a = {a}, w_min = {w_min}"
                );
            }
        }
    }
}

#[test]
fn the_tightened_threshold_never_excludes_a_document_that_could_reach_tau() {
    let m = 32;
    for metric in [
        Metric::Dot,
        Metric::Hamming,
        Metric::Jaccard,
        Metric::Cosine,
    ] {
        for w_min in 0..=48u32 {
            for w in w_min.max(1)..=48u32 {
                for a in 0..=w.min(m) {
                    let tau = metric.score(a, w, m);
                    let need = metric.min_intersection_with(tau, m, w_min);
                    assert!(
                        need <= a,
                        "{metric:?}: tau from a = {a}, w = {w}, w_min = {w_min} demands \
                         {need}, excluding its own source"
                    );
                }
            }
        }
    }
}

#[test]
fn equality_and_ordering_agree_on_every_pair() {
    // `Ord`'s contract: `a.cmp(b) == Equal` exactly when `a == b`. This was
    // broken -- `PartialEq` derived and structural, `cmp` cross-multiplied and
    // semantic -- so two spellings of one score compared `Equal` and were not
    // `==`. It surfaced only because a test happened to produce `4/80` on one
    // side and `2/40` on the other; no production path compares scores with
    // `==`, which is exactly why nothing found it.
    // Grouped by kind, because `cmp` across kinds is a documented programming
    // error and asserts in debug -- the contract binds within a metric, which is
    // the only place two scores are ever compared. Writing this as one flat loop
    // tripped that assertion, which is the guard working.
    let ints: Vec<Score> = (0..8i64).map(Score::Int).collect();
    let mut ratios = Vec::new();
    let mut sqs = Vec::new();
    for n in 0..8u64 {
        for d in 1..8u64 {
            ratios.push(Score::ratio(n, d));
            ratios.push(Score::ratio(n * 2, d * 2));
            sqs.push(Score::ratio_sq(u128::from(n), u128::from(d)));
            sqs.push(Score::ratio_sq(u128::from(n) * 3, u128::from(d) * 3));
        }
    }
    let mut pairs = 0usize;
    for group in [&ints, &ratios, &sqs] {
        for a in group {
            for b in group {
                pairs += 1;
                assert_eq!(
                    a.cmp(b) == std::cmp::Ordering::Equal,
                    a == b,
                    "cmp and eq disagree on {a:?} against {b:?}"
                );
            }
        }
    }
    assert!(pairs > 10_000, "only {pairs} pairs compared");

    // Across kinds the requirement is weaker and different: equality must be
    // false, and asking must not reach the assertion above.
    for a in &ints {
        for b in ratios.iter().chain(sqs.iter()) {
            assert_ne!(a, b, "scores of different kinds are never equal");
        }
    }
}

#[test]
fn an_unreduced_score_equals_its_reduced_form() {
    // The case that started it, stated directly rather than as a side effect.
    assert_eq!(Score::ratio(1, 4), Score::ratio(2, 8));
    assert_eq!(Score::ratio_sq(2, 40), Score::ratio_sq(4, 80));
    // And different values remain different.
    assert_ne!(Score::ratio(1, 4), Score::ratio(2, 7));
    // Scores of different kinds are never equal, and asking does not trip the
    // debug assertion that guards against comparing metrics.
    assert_ne!(Score::Int(0), Score::ratio(0, 1));
}
