//! The oracle's own properties.
//!
//! The oracle is what every kernel is checked against, so it is the one thing
//! with nothing above it to check *it*. What can be done instead is to assert
//! the invariants its answer must satisfy regardless of implementation -- and to
//! cross-check the three `intersect` arms against each other, since they are
//! specializations of one definition.

use std::collections::BTreeSet;

use haiiie_core::{CodeRef, DocId, Metric};
use haiiie_testkit::{Corpus, Shape, oracle};

const SHAPES: [Shape; 5] = [
    Shape::Balanced,
    Shape::Sparse,
    Shape::Clustered,
    Shape::Degenerate,
    Shape::OrdinalRuns,
];

fn corpus(shape: Shape, dims: u32, n: usize) -> Corpus {
    Corpus::generate(0xC0FFEE ^ dims as u64, dims, n, shape)
}

#[test]
fn the_three_intersect_arms_agree() {
    // Dense/dense, sparse/dense and sparse/sparse are one definition written
    // three ways. A disagreement here would make every other test's oracle
    // depend on which representation the corpus happened to use.
    for shape in SHAPES {
        let c = corpus(shape, 256, 32);
        for i in 0..c.len() {
            for j in 0..c.len() {
                let (a, b) = (c.code(i), c.code(j));
                let sa: Vec<u32> = positions(&c.codes[i]);
                let sb: Vec<u32> = positions(&c.codes[j]);
                let dense = a.intersect(b);
                assert_eq!(dense, CodeRef::Sparse(&sa).intersect(b), "sparse/dense");
                assert_eq!(
                    dense,
                    CodeRef::Sparse(&sa).intersect(CodeRef::Sparse(&sb)),
                    "sparse/sparse"
                );
                assert_eq!(dense, a.intersect(CodeRef::Sparse(&sb)), "dense/sparse");
            }
        }
    }
}

fn positions(words: &[u64]) -> Vec<u32> {
    let mut v = Vec::new();
    for (w, word) in words.iter().enumerate() {
        let mut x = *word;
        while x != 0 {
            v.push((w as u32) * 64 + x.trailing_zeros());
            x &= x - 1;
        }
    }
    v
}

#[test]
fn top_k_is_a_prefix_of_top_k_plus_one() {
    for shape in SHAPES {
        let c = corpus(shape, 128, 64);
        let q = c.code(0);
        for metric in [
            Metric::Dot,
            Metric::Hamming,
            Metric::Jaccard,
            Metric::Cosine,
        ] {
            for k in 0..12 {
                let a = oracle::top_k(&c, q, metric, k, None, None);
                let b = oracle::top_k(&c, q, metric, k + 1, None, None);
                assert_eq!(a, b[..a.len()], "{shape:?}/{metric:?} at k = {k}");
            }
        }
    }
}

#[test]
fn ties_break_on_ascending_id_and_are_deterministic() {
    // Every code identical, so every score ties: the result is forced to be the
    // first k ids in order. This is the only tie rule the crate promises.
    let c = Corpus {
        dims: 64,
        codes: vec![vec![0xFFFF_0000_FFFF_0000u64]; 16],
    };
    let hits = oracle::top_k(&c, c.code(0), Metric::Jaccard, 5, None, None);
    assert_eq!(
        hits.iter().map(|h| h.id).collect::<Vec<_>>(),
        (0..5).map(DocId).collect::<Vec<_>>()
    );
}

#[test]
fn a_filter_composes_as_set_intersection() {
    // This is the property that will catch a bad pruning bound at M5: a bound
    // that is too tight removes a document this construction keeps.
    for shape in SHAPES {
        let c = corpus(shape, 128, 48);
        let q = c.code(1);
        let filter: BTreeSet<usize> = (0..c.len()).filter(|i| i % 3 == 0).collect();
        for metric in [
            Metric::Dot,
            Metric::Hamming,
            Metric::Jaccard,
            Metric::Cosine,
        ] {
            let filtered = oracle::top_k(&c, q, metric, 6, None, Some(&filter));
            let by_hand: Vec<_> = oracle::top_k(&c, q, metric, c.len(), None, None)
                .into_iter()
                .filter(|h| filter.contains(&(h.id.get() as usize)))
                .take(6)
                .collect();
            assert_eq!(filtered, by_hand, "{shape:?}/{metric:?}");
        }
    }
}

#[test]
fn hamming_identity_holds_for_every_hit() {
    for shape in SHAPES {
        let c = corpus(shape, 192, 40);
        let q = c.code(3);
        let m = q.weight();
        for h in oracle::top_k(&c, q, Metric::Hamming, c.len(), None, None) {
            assert_eq!(
                Metric::hamming_distance(h.inter, h.weight, m),
                m + h.weight - 2 * h.inter
            );
            assert!(h.inter <= h.weight.min(m), "a <= min(w, m) must hold");
        }
    }
}

#[test]
fn degenerate_queries_do_not_panic_and_stay_ordered() {
    let c = corpus(Shape::Degenerate, 64, 24);
    let zero = vec![0u64; 1];
    let ones = vec![u64::MAX; 1];
    for q in [CodeRef::Dense(&zero), CodeRef::Dense(&ones)] {
        for metric in [
            Metric::Dot,
            Metric::Hamming,
            Metric::Jaccard,
            Metric::Cosine,
        ] {
            let hits = oracle::top_k(&c, q, metric, 8, None, None);
            assert!(hits.windows(2).all(|w| w[0].rank_key() <= w[1].rank_key()));
        }
    }
}

#[test]
fn the_generators_actually_reach_their_boundaries() {
    // A generator that silently produced one shape would make every suite above
    // it vacuous, and nothing else in the tree would notice.
    let deg = corpus(Shape::Degenerate, 128, 40);
    assert!(
        deg.codes.iter().any(|c| c.iter().all(|w| *w == 0)),
        "no empty code"
    );
    assert!(
        deg.codes
            .iter()
            .any(|c| c.iter().map(|w| w.count_ones()).sum::<u32>() == 128),
        "no full code"
    );

    let sparse = corpus(Shape::Sparse, 512, 32);
    let avg: f64 = sparse
        .codes
        .iter()
        .map(|c| f64::from(c.iter().map(|w| w.count_ones()).sum::<u32>()))
        .sum::<f64>()
        / 32.0;
    assert!(avg < 32.0, "sparse shape is not sparse: {avg} bits of 512");

    let bal = corpus(Shape::Balanced, 512, 32);
    let avg_b: f64 = bal
        .codes
        .iter()
        .map(|c| f64::from(c.iter().map(|w| w.count_ones()).sum::<u32>()))
        .sum::<f64>()
        / 32.0;
    assert!(
        (200.0..312.0).contains(&avg_b),
        "balanced shape is off: {avg_b}"
    );
}
