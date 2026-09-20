//! The edges of the accepted range, exercised rather than constructed.
//!
//! # Why this file exists
//!
//! `Index::create` accepted an index sixteen times wider than one that can hold
//! a document, and the test guarding that bound never noticed, because it
//! asserted only that `create` returned `Ok`. An accepted neighbour that is
//! merely constructed says nothing: **the point of pairing a refusal with its
//! neighbour is to show the boundary is in the right place, and a value that
//! cannot be used is not on the accepted side of it.**
//!
//! So each edge here is put, committed, filtered and scored, and the answer is
//! checked: the narrowest geometry, the widest, the largest attribute term, and
//! the largest id.
//!
//! # Which test owns which half
//!
//! Three of those four bounds are **asked of the engine**, not written down. Two
//! were literals -- `65 536` and `1 048 575` -- which are the right answers today
//! and stop being boundaries the moment a bound moves, leaving probes that pass
//! and a header still calling them the widest and the largest. The refusals
//! carry the engine's own number, so asking for an impossible width or term and
//! reading it back cannot drift.
//!
//! That means this file deliberately **does not** pin where the boundary is: it
//! follows it. Halving the dimension bound leaves these tests green, by design.
//! `key_layout.rs` owns the location -- it asserts the refusal at one past the
//! bound and that the accepted neighbour holds a document -- and it catches that
//! same sabotage. Verified rather than asserted: the halving injection is caught
//! there and passes here.
//!
//! Splitting it that way is the point. A test that both locates a boundary and
//! exercises it has to name the number, and naming the number is what made the
//! two literals rot silently.

use haiiie_core::{CodeRef, DocId, Filter, Index, Metric, PathHint, YesnoStore};

fn probe(label: &str, dims: u32, id: u64, term: u32, full: bool) {
    let t0 = std::time::Instant::now();
    let dir = tempfile::tempdir().expect("tempdir");
    let idx = Index::create(
        YesnoStore::open(dir.path().join("db")).expect("open"),
        1,
        dims,
    )
    .expect("create");
    let words = (idx.meta().row_bits / 64) as usize;
    let mut code = vec![0u64; words];
    code[0] |= 1;
    {
        let mut w = idx.writer();
        w.put(DocId(id), CodeRef::Dense(&code)).expect("put");
        w.attr(DocId(id), term).expect("attr");
        w.commit().expect("commit");
    }
    idx.refresh_stats().expect("stats");
    // **The high-id case searches once, not twelve times.** A scan visits every
    // block from zero to the highest id, so at the top of the range one query
    // costs about five seconds -- twelve of them is a minute, which is twice the
    // whole churn suite. One is enough to show the largest accepted id answers
    // correctly; the path and metric matrix is the other cases' job, and they
    // are milliseconds.
    let paths: &[PathHint] = if full {
        &[PathHint::Gather, PathHint::DenseScan, PathHint::Inverted]
    } else {
        &[PathHint::Gather]
    };
    let metrics: &[Metric] = if full {
        &[
            Metric::Hamming,
            Metric::Jaccard,
            Metric::Cosine,
            Metric::Dot,
        ]
    } else {
        &[Metric::Hamming]
    };
    for &path in paths {
        for &metric in metrics {
            let hits = idx
                .search()
                .code(CodeRef::Dense(&code))
                .metric(metric)
                .k(5)
                .filter(Filter::Term(term))
                .path(path)
                .execute()
                .expect("search")
                .hits;
            assert_eq!(hits.len(), 1, "{label} {path:?} {metric:?}: {hits:?}");
            assert_eq!(hits[0].id.get(), id, "{label} {path:?} {metric:?}");
            assert_eq!(
                (hits[0].inter, hits[0].weight),
                (1, 1),
                "{label} {path:?} {metric:?}"
            );
        }
    }
    eprintln!("{label}: ok in {:?}", t0.elapsed());
}

/// The widest index the engine accepts, **asked rather than assumed**.
///
/// This was the literal 65 536, which is the answer today and stops being a
/// boundary the moment the bound moves -- leaving a probe that still passes and
/// a header that still calls it the widest. The refusal carries the engine's own
/// number, so asking for an impossible width and reading the answer cannot
/// drift.
fn widest_dims() -> u32 {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = YesnoStore::open(dir.path().join("probe")).expect("open");
    match Index::create(store, 9, u32::MAX) {
        Err(haiiie_core::Error::TooManyDimensions { max, .. }) => max,
        other => panic!(
            "u32::MAX dimensions must be refused, got {:?}",
            other.map(|_| ())
        ),
    }
}

/// The largest attribute term the engine accepts, asked the same way.
fn largest_term() -> u32 {
    let dir = tempfile::tempdir().expect("tempdir");
    let idx = Index::create(
        YesnoStore::open(dir.path().join("probe")).expect("open"),
        9,
        64,
    )
    .expect("create");
    let mut w = idx.writer();
    match w.attr(DocId(0), u32::MAX) {
        Err(haiiie_core::Error::AttrTermTooLarge { max, .. }) => max,
        other => panic!("a u32::MAX term must be refused, got {other:?}"),
    }
}

#[test]
fn edges_of_the_accepted_range_actually_work() {
    probe("one dimension", 1, 0, 0, true);
    probe("widest index", widest_dims(), 0, 0, true);
    probe("largest term", 64, 0, largest_term(), true);
    let dir = tempfile::tempdir().expect("tempdir");
    let idx = Index::create(YesnoStore::open(dir.path().join("x")).expect("o"), 1, 64).expect("c");
    probe("largest id", 64, idx.max_doc_id(), 0, false);
}
