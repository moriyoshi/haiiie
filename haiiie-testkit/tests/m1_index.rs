//! M1's central claims, checked four ways.
//!
//! A search is defined by three things that could each be wrong independently:
//! the store it reads through, the path it takes, and the scoring itself. So the
//! suite crosses them -- `MemStore` against `YesnoStore`, `Gather` against
//! `DenseScan`, and both against the brute-force oracle -- rather than checking
//! one configuration and assuming the rest follow.
//!
//! The store comparison is the one that makes the abstraction worth having. A
//! trait with a single implementation is under-determined: whatever that
//! implementation does becomes the specification, including its bugs.

use std::collections::BTreeSet;

use haiiie_core::{CodeRef, DocId, Filter, Index, Metric, Path, PathHint, SetStore, YesnoStore};
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
    let idx = Index::create(store, 7, c.dims).expect("create");
    let mut w = idx.writer();
    for i in 0..c.len() {
        w.put(DocId(i as u64), c.code(i)).expect("put");
    }
    w.commit().expect("commit");
    idx
}

/// Every path, on every shape, against the oracle -- and the two paths must
/// agree with each other exactly, not merely both look plausible.
#[test]
fn both_paths_match_the_oracle_on_every_shape() {
    for shape in SHAPES {
        let c = Corpus::generate(11, 128, 40, shape);
        let idx = ingest(MemStore::new(), &c);
        for metric in METRICS {
            for k in [1usize, 5, 40] {
                let want = oracle::top_k(&c, c.code(0), metric, k, None, None);
                let g = idx
                    .search()
                    .code(c.code(0))
                    .metric(metric)
                    .k(k)
                    .path(PathHint::Gather)
                    .execute()
                    .expect("gather");
                let d = idx
                    .search()
                    .code(c.code(0))
                    .metric(metric)
                    .k(k)
                    .path(PathHint::DenseScan)
                    .execute()
                    .expect("scan");
                assert_eq!(g.hits, want, "gather vs oracle: {shape:?}/{metric:?}/k={k}");
                assert_eq!(d.hits, want, "scan vs oracle: {shape:?}/{metric:?}/k={k}");
                assert_eq!(g.path, Path::Gather);
                assert_eq!(d.path, Path::DenseScan);
            }
        }
    }
}

/// The property that makes `SetStore` earn its keep.
#[test]
fn the_two_stores_return_byte_identical_hits() {
    let dir = tempfile::tempdir().expect("tempdir");
    for shape in SHAPES {
        let c = Corpus::generate(23, 192, 24, shape);
        let mem = ingest(MemStore::new(), &c);
        let path = dir.path().join(format!("{shape:?}"));
        let yes = ingest(YesnoStore::open(&path).expect("open"), &c);

        for metric in METRICS {
            let a = mem.search().code(c.code(1)).metric(metric).k(7).execute();
            let b = yes.search().code(c.code(1)).metric(metric).k(7).execute();
            assert_eq!(
                a.expect("mem").hits,
                b.expect("yesno").hits,
                "{shape:?}/{metric:?}"
            );
        }
    }
}

/// Reopen before asserting: a live handle answers from the memtable and would
/// hide a persistence bug entirely. This test found the metadata-length bug at
/// M1 and nothing else did.
///
/// **Blocked at M2 on an upstream crash** ( `yesno-core/src/index/node.rs:304`,
/// leaf suffix widths 10 and 12; reproduction at
/// `.agents-workspace/tmp/ksuf-repro` ). Worth recording precisely how it
/// arrived: this test passed at M1 and began crashing at M2 with **no change to
/// any read path**. Ingest gained two more key kinds, the leaf's required
/// suffix width crossed from 8 to 10, and the defect was waiting there the
/// whole time. A data-dependent crash does not announce which change exposed
/// it, so do not read a newly red durability test as evidence about the change
/// that preceded it.
#[test]
fn results_survive_a_checkpoint_and_reopen() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("db");
    let c = Corpus::generate(31, 256, 30, Shape::Balanced);

    let before = {
        let idx = ingest(YesnoStore::open(&path).expect("open"), &c);
        idx.store().flush().expect("checkpoint");
        idx.search()
            .code(c.code(2))
            .metric(Metric::Hamming)
            .k(9)
            .execute()
            .expect("search")
            .hits
    };

    let reopened = Index::open(YesnoStore::open(&path).expect("reopen"), 7).expect("open index");
    assert_eq!(reopened.meta(), haiiie_core::IndexMeta::new(c.dims));
    let after = reopened
        .search()
        .code(c.code(2))
        .metric(Metric::Hamming)
        .k(9)
        .execute()
        .expect("search")
        .hits;
    assert_eq!(before, after);
    assert_eq!(reopened.len().expect("len"), c.len() as u64);
}

/// A filter composes as set intersection, on both paths.
#[test]
fn filters_compose_as_intersection() {
    let c = Corpus::generate(41, 128, 36, Shape::Balanced);
    let idx = ingest(MemStore::new(), &c);
    {
        let mut w = idx.writer();
        for i in (0..c.len()).step_by(3) {
            w.attr(DocId(i as u64), 1).expect("attr");
        }
        w.commit().expect("attrs");
    }
    let admitted: BTreeSet<usize> = (0..c.len()).step_by(3).collect();

    for metric in METRICS {
        let want = oracle::top_k(&c, c.code(0), metric, 5, None, Some(&admitted));
        for hint in [PathHint::Gather, PathHint::DenseScan, PathHint::Inverted] {
            let got = idx
                .search()
                .code(c.code(0))
                .metric(metric)
                .k(5)
                .filter(Filter::Term(1))
                .path(hint)
                .execute()
                .expect("search");
            assert_eq!(got.hits, want, "{metric:?} {hint:?}");
            // `scored` counts documents whose exact score was computed, and the
            // paths legitimately differ: a forward scan scores every admitted
            // document, while the inverted path scores only what the descent
            // and the refinement select. What must hold for **all** of them is
            // that it never exceeds the admitted set -- scoring more than the
            // filter allows would mean a document escaped it.
            assert!(
                got.scored <= admitted.len() as u64,
                "{metric:?} {hint:?} scored {} of {} admitted",
                got.scored,
                admitted.len()
            );
            if hint != PathHint::Inverted {
                assert_eq!(
                    got.scored,
                    admitted.len() as u64,
                    "a forward scan must score all"
                );
            }
        }
    }
    // Not(Term) is complemented within the live set, not the ordinal universe.
    let rest = idx
        .search()
        .code(c.code(0))
        .filter(Filter::Not(Box::new(Filter::Term(1))))
        .k(usize::MAX)
        .execute()
        .expect("not");
    assert_eq!(rest.hits.len(), c.len() - admitted.len());
}

/// A delete removes the document and leaves no trace that could score.
#[test]
fn a_deleted_document_cannot_be_returned_or_scored() {
    let c = Corpus::generate(53, 64, 12, Shape::Balanced);
    let idx = ingest(MemStore::new(), &c);
    let mut w = idx.writer();
    w.delete(DocId(3)).expect("delete");
    w.commit().expect("delete");

    let hits = idx
        .search()
        .code(c.code(3))
        .k(usize::MAX)
        .execute()
        .expect("search");
    assert!(hits.hits.iter().all(|h| h.id != DocId(3)));
    assert_eq!(hits.hits.len(), c.len() - 1);
    assert_eq!(idx.len().expect("len"), c.len() as u64 - 1);
    // The forward row is gone, not merely unlisted: nothing survives to be
    // scored if the ordinal were ever revived.
    let snap = idx.store().snapshot().expect("snap");
    let addr = idx.row_addr(DocId(3));
    let ords = haiiie_core::SetSnapshot::load(&snap, idx.keys().forward()).expect("load");
    let end = addr.base + u64::from(idx.meta().row_bits);
    assert!(!ords.iter().any(|&o| (addr.base..end).contains(&o)));
}

/// A put over an existing document replaces its code rather than merging.
#[test]
fn a_replace_is_not_a_merge() {
    let c = Corpus::generate(67, 64, 4, Shape::Balanced);
    let idx = ingest(MemStore::new(), &c);
    let empty = [0u64; 1];
    let mut w = idx.writer();
    w.put(DocId(1), CodeRef::Dense(&empty)).expect("put");
    w.commit().expect("commit");

    let hits = idx
        .search()
        .code(c.code(1))
        .k(usize::MAX)
        .execute()
        .expect("s");
    let h = hits
        .hits
        .iter()
        .find(|h| h.id == DocId(1))
        .expect("present");
    assert_eq!(h.weight, 0, "the old code survived the replace");
    assert_eq!(h.inter, 0);
}

/// A model identity is part of the durable index header, rather than state held
/// only by the creating handle. Without the reopen, a missing write is invisible.
#[test]
fn a_model_identity_survives_a_checkpoint_and_reopen() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("model-bound-db");
    let identity = [0x3c; 32];
    {
        let idx =
            Index::create_with_model_id(YesnoStore::open(&path).expect("open"), 9, 512, identity)
                .expect("create model-bound index");
        idx.store().flush().expect("checkpoint");
    }
    let reopened = Index::open(YesnoStore::open(&path).expect("reopen"), 9).expect("open index");
    assert_eq!(reopened.meta().model_id, Some(identity));
}
