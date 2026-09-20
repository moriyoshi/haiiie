//! Caller attributes: the half of the API that removes them, and what a
//! replacement must leave alone.
//!
//! # Why this file exists
//!
//! `Writer::attr` is exercised by four suites. `Writer::unattr` had **no mention
//! in any test** -- found by auditing public items for callers. It is a public
//! mutator that deletes data, and its failure is a filter admitting a document
//! it should exclude: a wrong answer with no error, no missing key and nothing
//! for a differential suite to disagree with, because the oracle is told the
//! same filter.
//!
//! The second property here is the one nobody wrote down. Attributes are the
//! **caller's**, not derived from the code, so replacing a document's code must
//! leave them untouched. `put` clears and rewrites `DIM`, `ZPLANE` and the
//! forward row and deliberately does not touch `ATTR` -- correct, and true only
//! by the absence of a line. A change that swept attributes into the clearing
//! loop would look tidy, break no other test, and silently drop documents out of
//! every filtered query.

use haiiie_core::{CodeRef, DocId, Filter, Index, Metric, PathHint, YesnoStore};

const DIMS: u32 = 128;
const WORDS: usize = 2;

fn code(bits: &[u32]) -> Vec<u64> {
    let mut v = vec![0u64; WORDS];
    for &b in bits {
        v[(b as usize) >> 6] |= 1u64 << (b & 63);
    }
    v
}

fn admitted(idx: &Index<YesnoStore>, f: Filter) -> Vec<u64> {
    let q = code(&[0, 1, 2]);
    let mut ids: Vec<u64> = idx
        .search()
        .code(CodeRef::Dense(&q))
        .metric(Metric::Hamming)
        .k(64)
        .filter(f)
        .path(PathHint::Gather)
        .execute()
        .expect("search")
        .hits
        .iter()
        .map(|h| h.id.get())
        .collect();
    ids.sort_unstable();
    ids
}

fn build(dir: &tempfile::TempDir) -> Index<YesnoStore> {
    let idx = Index::create(
        YesnoStore::open(dir.path().join("db")).expect("open"),
        1,
        DIMS,
    )
    .expect("create");
    {
        let mut w = idx.writer();
        for i in 0..8u64 {
            w.put(DocId(i), CodeRef::Dense(&code(&[0, 1, 2])))
                .expect("put");
            // Even ids carry term 1, all ids carry term 7.
            if i % 2 == 0 {
                w.attr(DocId(i), 1).expect("attr");
            }
            w.attr(DocId(i), 7).expect("attr");
        }
        w.commit().expect("commit");
    }
    idx.refresh_stats().expect("stats");
    idx
}

#[test]
fn out_of_range_filter_terms_are_refused_before_key_construction() {
    let dir = tempfile::tempdir().expect("tempdir");
    let idx = build(&dir);
    let max = haiiie_core::KeySpace::INDEX_MAX as u32;
    let query = code(&[0, 1, 2]);

    assert_eq!(admitted(&idx, Filter::Term(7)), (0..8).collect::<Vec<_>>());
    assert_eq!(idx.search().filter(Filter::Term(max)).count().unwrap(), 0);
    for term in [max + 1, (1 << 24) + 7] {
        for filter in [
            Filter::Term(term),
            Filter::Or(vec![Filter::None, Filter::Term(term)]),
            Filter::Not(Box::new(Filter::Term(term))),
        ] {
            let check = |error| {
                assert!(
                    matches!(
                        &error,
                        haiiie_core::Error::AttrTermTooLarge { term: t, max: m }
                            if *t == term && *m == max
                    ),
                    "wrong refusal for term {term}: {error:?}"
                );
            };
            check(idx.search().filter(filter.clone()).count().unwrap_err());
            check(
                idx.search()
                    .code(CodeRef::Dense(&query))
                    .metric(Metric::Hamming)
                    .k(8)
                    .filter(filter.clone())
                    .path(PathHint::Inverted)
                    .execute()
                    .unwrap_err(),
            );
            check(idx.search().filter(filter).explain().unwrap_err());
        }
    }
}

#[test]
fn empty_indexes_still_refuse_out_of_range_filter_terms() {
    let dir = tempfile::tempdir().expect("tempdir");
    let idx = Index::create(
        YesnoStore::open(dir.path().join("empty")).expect("open"),
        1,
        DIMS,
    )
    .expect("create");
    let invalid = Filter::Term((1 << 24) + 7);
    let scorer = |_: &[u64]| 0i64;
    assert!(matches!(
        idx.search().filter(invalid.clone()).count(),
        Err(haiiie_core::Error::AttrTermTooLarge { .. })
    ));
    assert!(matches!(
        idx.search().filter(invalid.clone()).execute(),
        Err(haiiie_core::Error::AttrTermTooLarge { .. })
    ));
    assert!(matches!(
        idx.row_search(&scorer).filter(invalid).execute(),
        Err(haiiie_core::Error::AttrTermTooLarge { .. })
    ));
}

#[test]
fn unattr_removes_exactly_the_one_attribute() {
    let dir = tempfile::tempdir().expect("tempdir");
    let idx = build(&dir);
    assert_eq!(admitted(&idx, Filter::Term(1)), vec![0, 2, 4, 6]);

    {
        let mut w = idx.writer();
        w.unattr(DocId(4), 1).expect("unattr");
        w.commit().expect("commit");
    }

    // Removed from term 1 ...
    assert_eq!(
        admitted(&idx, Filter::Term(1)),
        vec![0, 2, 6],
        "unattr did not detach the term"
    );
    // ... and from nothing else. A removal that cleared the whole key, or the
    // document's other terms, passes the assertion above and fails this one.
    assert_eq!(
        admitted(&idx, Filter::Term(7)),
        (0..8).collect::<Vec<_>>(),
        "unattr disturbed an unrelated term"
    );
    // The document itself is untouched: only the attribute was removed.
    assert_eq!(admitted(&idx, Filter::All), (0..8).collect::<Vec<_>>());
}

#[test]
fn unattr_on_a_document_that_never_had_the_term_changes_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let idx = build(&dir);
    {
        let mut w = idx.writer();
        w.unattr(DocId(3), 1).expect("unattr"); // 3 is odd and never carried term 1
        w.unattr(DocId(0), 99).expect("unattr"); // a term nothing carries
        w.commit().expect("commit");
    }
    assert_eq!(admitted(&idx, Filter::Term(1)), vec![0, 2, 4, 6]);
    assert_eq!(admitted(&idx, Filter::Term(7)), (0..8).collect::<Vec<_>>());
}

#[test]
fn replacing_a_document_keeps_its_attributes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let idx = build(&dir);
    {
        let mut w = idx.writer();
        // A genuinely different code, so the diffing path does real work.
        w.put(DocId(2), CodeRef::Dense(&code(&[64, 65, 66])))
            .expect("put");
        w.commit().expect("commit");
    }
    idx.refresh_stats().expect("stats");

    assert_eq!(
        admitted(&idx, Filter::Term(1)),
        vec![0, 2, 4, 6],
        "a replacement dropped the document's attributes"
    );
    assert_eq!(admitted(&idx, Filter::Term(7)), (0..8).collect::<Vec<_>>());
}

#[test]
fn attributes_survive_a_reopen() {
    let dir = tempfile::tempdir().expect("tempdir");
    {
        let idx = build(&dir);
        let mut w = idx.writer();
        w.unattr(DocId(6), 1).expect("unattr");
        w.commit().expect("commit");
    }
    // Reopened: a live handle answers from the memtable and would hide a
    // persistence bug in either direction -- the attribute or its removal.
    let idx =
        Index::open(YesnoStore::open(dir.path().join("db")).expect("reopen"), 1).expect("open");
    idx.refresh_stats().expect("stats");
    assert_eq!(admitted(&idx, Filter::Term(1)), vec![0, 2, 4]);
    assert_eq!(admitted(&idx, Filter::Term(7)), (0..8).collect::<Vec<_>>());
}

/// The same property on the **other** write path.
///
/// `put` differences against the stored row when it can, and falls back to
/// clearing every `DIM` and `ZPLANE` blindly when the id was already written in
/// this batch -- its old row then lives only in the batch, where no snapshot can
/// see it. Those are two separate clearing loops, and a change sweeping
/// attributes into one of them is invisible to a test that only drives the
/// other.
///
/// Found by sabotage: an injected attribute-clearing loop in the blind path went
/// uncaught, because every replacement above is diffable and never enters it.
/// The sabotage was miscalibrated rather than the test toothless, and the gap it
/// exposed was real.
#[test]
fn a_within_batch_replacement_keeps_attributes_too() {
    let dir = tempfile::tempdir().expect("tempdir");
    let idx = build(&dir);
    {
        let mut w = idx.writer();
        // Two puts of one id in a single batch: the second takes the blind path.
        w.put(DocId(2), CodeRef::Dense(&code(&[64, 65])))
            .expect("put");
        w.put(DocId(2), CodeRef::Dense(&code(&[96, 97])))
            .expect("put");
        w.commit().expect("commit");
    }
    idx.refresh_stats().expect("stats");
    assert_eq!(
        admitted(&idx, Filter::Term(1)),
        vec![0, 2, 4, 6],
        "the blind-clearing path dropped the document's attributes"
    );
    assert_eq!(admitted(&idx, Filter::Term(7)), (0..8).collect::<Vec<_>>());
}
