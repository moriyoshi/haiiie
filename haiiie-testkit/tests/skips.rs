//! Blocks the scan skips, and the statistic that counts them.
//!
//! # Why this file exists
//!
//! `ScanStats::blocks_skipped` was **never non-zero in any test**. Two suites
//! assert it equals zero on an empty index, which is true and says nothing
//! about whether the counter can move -- the same shape as a fallback whose
//! only observed state is the one where it does nothing. A statistic that has
//! only ever been observed at rest is not evidence that it counts anything.
//!
//! Found by instrumenting the defensive branches of the scan and reading the
//! zeros, on upstream's method. Two of seven never fired.
//!
//! # The two ways a block is skipped are different mechanisms
//!
//! A block can carry no live documents at all -- an id gap, which a caller
//! makes by assigning sparse ids, and which costs one `load_block` to discover.
//! Or it can be live and admit nothing under the filter, which costs the filter
//! evaluation as well. They are separate `return` sites and a test of one says
//! nothing about the other, so both are here.
//!
//! # `Filter::None` had no test at all
//!
//! It is a public variant, reachable over the wire ( the gRPC layer maps the
//! protobuf `none` clause onto it ), and nothing anywhere exercised it. It is
//! the cheapest possible way to drive the admitted-nothing skip, and a search
//! that returns results under it would be a straightforward wrong answer.

use haiiie_core::{CodeRef, DocId, Filter, Index, Metric, PathHint, YesnoStore};

const DIMS: u32 = 256;
const WORDS: usize = 4;

fn code(bits: &[u32]) -> Vec<u64> {
    let mut v = vec![0u64; WORDS];
    for &b in bits {
        v[(b as usize) >> 6] |= 1u64 << (b & 63);
    }
    v
}

fn index_with(ids: &[u64]) -> (tempfile::TempDir, Index<YesnoStore>) {
    let dir = tempfile::tempdir().expect("tempdir");
    let idx = Index::create(
        YesnoStore::open(dir.path().join("db")).expect("open"),
        1,
        DIMS,
    )
    .expect("create");
    {
        let mut w = idx.writer();
        for (i, &id) in ids.iter().enumerate() {
            let base = ((i as u32 % 80) * 3) % (DIMS - 3);
            w.put(
                DocId(id),
                CodeRef::Dense(&code(&[base, base + 1, base + 2])),
            )
            .expect("put");
        }
        w.commit().expect("commit");
    }
    idx.refresh_stats().expect("stats");
    (dir, idx)
}

#[test]
fn a_block_with_no_live_documents_is_skipped_and_counted() {
    // Ids in block 0 and block 3, so blocks 1 and 2 hold nothing at all.
    let (_dir, idx) = index_with(&[0, 1, 200_000]);
    let q = code(&[0, 1, 2]);
    for path in [PathHint::Inverted, PathHint::Gather, PathHint::DenseScan] {
        let got = idx
            .search()
            .code(CodeRef::Dense(&q))
            .metric(Metric::Hamming)
            .k(10)
            .path(path)
            .execute()
            .expect("search");
        assert_eq!(
            got.stats.blocks_skipped, 2,
            "{path:?}: blocks 1 and 2 hold no live document and must be skipped"
        );
        assert_eq!(
            got.stats.blocks_visited, 2,
            "{path:?}: blocks 0 and 3 carry documents"
        );
        assert_eq!(
            got.hits.len(),
            3,
            "{path:?}: every document is still returned"
        );
    }
}

#[test]
fn a_filter_admitting_nothing_skips_every_block_and_returns_nothing() {
    let (_dir, idx) = index_with(&[0, 1, 2, 200_000]);
    let q = code(&[0, 1, 2]);
    for path in [PathHint::Inverted, PathHint::Gather, PathHint::DenseScan] {
        let got = idx
            .search()
            .code(CodeRef::Dense(&q))
            .metric(Metric::Hamming)
            .k(10)
            .filter(Filter::None)
            .path(path)
            .execute()
            .expect("search");
        assert!(
            got.hits.is_empty(),
            "{path:?}: Filter::None admits nothing, so nothing can be returned"
        );
        // Every block that holds a live document is visited and admits nothing;
        // the two empty ones are skipped for the other reason. Both land here.
        assert_eq!(
            got.stats.blocks_visited, 0,
            "{path:?}: no block can be scored when none admits a document"
        );
        assert!(
            got.stats.blocks_skipped >= 2,
            "{path:?}: skipped {} block(s), expected every block to skip",
            got.stats.blocks_skipped
        );
    }
}

/// The counter must distinguish the two, not merely be non-zero: a corpus with
/// no gaps and a filter that admits part of it skips only for the filter.
#[test]
fn a_dense_corpus_skips_only_for_the_filter() {
    let (_dir, idx) = index_with(&(0..8u64).collect::<Vec<_>>());
    let q = code(&[0, 1, 2]);
    let got = idx
        .search()
        .code(CodeRef::Dense(&q))
        .metric(Metric::Hamming)
        .k(10)
        .filter(Filter::Ids(vec![DocId(0), DocId(3)]))
        .path(PathHint::Gather)
        .execute()
        .expect("search");
    assert_eq!(
        got.stats.blocks_skipped, 0,
        "one block, and it admits two ids"
    );
    assert_eq!(got.stats.blocks_visited, 1);
    assert_eq!(got.hits.len(), 2);
}
