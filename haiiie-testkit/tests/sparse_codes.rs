//! Sparse codes through the engine: ingest, replacement, and query.
//!
//! # Why this file exists
//!
//! Instrumenting every `CodeRef::Sparse` arm in the engine against the whole
//! suite found **all three never taken**: the insert path, the differencing
//! path, and the query expansion. The only sparse coverage anywhere was the
//! oracle's pure `intersect` and a refusal test that errors before reaching any
//! of them.
//!
//! Sparse codes are an advertised use case -- tag signatures and SPLADE-like
//! vectors are the reason the variant exists -- and the gRPC layer maps a
//! protobuf `sparse.positions` list straight onto it, so this is a public input
//! with a wire representation and no engine test.
//!
//! # The property worth having is equivalence, not behaviour
//!
//! A sparse code and a dense code naming the same bits are the same code. So
//! every test here builds one corpus twice, in two indexes, and requires
//! byte-identical hits. That is stronger than asserting what sparse ingest
//! produces, and it cannot drift: the dense path is covered exhaustively
//! elsewhere, so it is the reference.
//!
//! # One deliberate asymmetry, pinned because it looks like a bug
//!
//! A **stored** code naming a position past `dims` is refused with
//! `DimensionMismatch`; a **query** naming one has it silently dropped. That is
//! the right way round -- a stored document outside the index's geometry is a
//! caller error, while a query mentioning a dimension this index does not have
//! is simply a term that matches nothing -- but the two live in different files
//! and neither states the other.

use haiiie_core::{CodeRef, DocId, Filter, Index, Metric, PathHint, YesnoStore};

const DIMS: u32 = 256;
const WORDS: usize = 4;

fn dense_of(positions: &[u32]) -> Vec<u64> {
    let mut v = vec![0u64; WORDS];
    for &b in positions {
        v[(b as usize) >> 6] |= 1u64 << (b & 63);
    }
    v
}

/// Ascending, unique positions -- the only shape the engine accepts, and what
/// the wire layer checks for rather than sorts.
fn positions(seed: u64, n: usize) -> Vec<u32> {
    let mut s = seed | 1;
    let mut v = Vec::new();
    for _ in 0..n {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        v.push((s % u64::from(DIMS)) as u32);
    }
    v.sort_unstable();
    v.dedup();
    v
}

struct Pair {
    _sparse_dir: tempfile::TempDir,
    _dense_dir: tempfile::TempDir,
    sparse: Index<YesnoStore>,
    dense: Index<YesnoStore>,
}

/// One corpus, ingested twice: once as sparse position lists, once as dense
/// words. Any divergence below is the sparse path's.
fn pair(docs: usize) -> (Pair, Vec<Vec<u32>>) {
    let sd = tempfile::tempdir().expect("tempdir");
    let dd = tempfile::tempdir().expect("tempdir");
    let sparse = Index::create(
        YesnoStore::open(sd.path().join("db")).expect("open"),
        1,
        DIMS,
    )
    .expect("create");
    let dense = Index::create(
        YesnoStore::open(dd.path().join("db")).expect("open"),
        1,
        DIMS,
    )
    .expect("create");

    let codes: Vec<Vec<u32>> = (0..docs).map(|i| positions(i as u64 + 7, 12)).collect();
    {
        let mut ws = sparse.writer();
        let mut wd = dense.writer();
        for (i, p) in codes.iter().enumerate() {
            ws.put(DocId(i as u64), CodeRef::Sparse(p))
                .expect("sparse put");
            wd.put(DocId(i as u64), CodeRef::Dense(&dense_of(p)))
                .expect("dense put");
        }
        ws.commit().expect("commit");
        wd.commit().expect("commit");
    }
    sparse.refresh_stats().expect("stats");
    dense.refresh_stats().expect("stats");
    (
        Pair {
            _sparse_dir: sd,
            _dense_dir: dd,
            sparse,
            dense,
        },
        codes,
    )
}

impl Pair {
    /// Both indexes, both query representations, every path and metric. Four
    /// combinations that must all agree.
    fn assert_agrees(&self, q: &[u32]) {
        let d = dense_of(q);
        for metric in [
            Metric::Dot,
            Metric::Hamming,
            Metric::Jaccard,
            Metric::Cosine,
        ] {
            for path in [PathHint::Gather, PathHint::DenseScan, PathHint::Inverted] {
                let run = |idx: &Index<YesnoStore>, c: CodeRef<'_>| {
                    idx.search()
                        .code(c)
                        .metric(metric)
                        .k(10)
                        .filter(Filter::All)
                        .path(path)
                        .execute()
                        .expect("search")
                        .hits
                };
                let reference = run(&self.dense, CodeRef::Dense(&d));
                assert_eq!(
                    run(&self.sparse, CodeRef::Sparse(q)),
                    reference,
                    "{metric:?} {path:?}: sparse index and sparse query"
                );
                assert_eq!(
                    run(&self.sparse, CodeRef::Dense(&d)),
                    reference,
                    "{metric:?} {path:?}: sparse index, dense query"
                );
                assert_eq!(
                    run(&self.dense, CodeRef::Sparse(q)),
                    reference,
                    "{metric:?} {path:?}: dense index, sparse query"
                );
            }
        }
    }
}

#[test]
fn a_sparse_code_is_the_same_code_as_its_dense_spelling() {
    let (p, codes) = pair(64);
    // A query taken from the corpus, so the top hit is an exact match, and one
    // built independently, so it is not.
    p.assert_agrees(&codes[0]);
    p.assert_agrees(&positions(999, 20));
}

#[test]
fn a_sparse_replacement_takes_the_differencing_path_correctly() {
    let (p, _) = pair(32);
    let replacement = positions(4242, 9);
    {
        let mut ws = p.sparse.writer();
        let mut wd = p.dense.writer();
        // An id the store already holds, so `put` differences rather than
        // inserting -- the sparse arm of that path, which nothing reached.
        ws.put(DocId(5), CodeRef::Sparse(&replacement))
            .expect("sparse replace");
        wd.put(DocId(5), CodeRef::Dense(&dense_of(&replacement)))
            .expect("dense replace");
        ws.commit().expect("commit");
        wd.commit().expect("commit");
    }
    p.sparse.refresh_stats().expect("stats");
    p.dense.refresh_stats().expect("stats");
    p.assert_agrees(&replacement);
}

#[test]
fn an_empty_sparse_code_is_a_code_of_weight_zero() {
    let (p, _) = pair(16);
    {
        let mut ws = p.sparse.writer();
        let mut wd = p.dense.writer();
        ws.put(DocId(1), CodeRef::Sparse(&[])).expect("sparse put");
        wd.put(DocId(1), CodeRef::Dense(&[0u64; WORDS]))
            .expect("dense put");
        ws.commit().expect("commit");
        wd.commit().expect("commit");
    }
    p.sparse.refresh_stats().expect("stats");
    p.dense.refresh_stats().expect("stats");
    p.assert_agrees(&positions(11, 8));
    // And an empty *query* is the degenerate case on the other side.
    p.assert_agrees(&[]);
}

#[test]
fn a_query_position_past_the_index_is_dropped_rather_than_refused() {
    let (p, _) = pair(16);
    let inside = positions(31, 6);
    let mut outside = inside.clone();
    outside.push(DIMS);
    outside.push(DIMS + 1_000);

    // The out-of-range positions contribute nothing, so the answer is the one
    // the in-range query gives. A stored code naming them is refused instead --
    // see `refusals.rs`, which asserts the other half of this asymmetry.
    let run = |q: &[u32]| {
        p.sparse
            .search()
            .code(CodeRef::Sparse(q))
            .metric(Metric::Hamming)
            .k(10)
            .path(PathHint::Inverted)
            .execute()
            .expect("search")
            .hits
    };
    assert_eq!(
        run(&outside),
        run(&inside),
        "a query dimension this index does not have must match nothing, not change the answer"
    );
}

/// The same defect reached from the dense side, where the caller cannot see it.
///
/// A dense code is `&[u64]`, so its width is a multiple of 64 while `dims` need
/// not be. At `dims = 200` the words carry 56 bits of padding that no document
/// row ever occupies -- ingest masks them off explicitly. A query with a bit set
/// there was counted in `m` and matched against nothing.
///
/// Checked on a **ratio** metric on purpose. For Hamming an inflated `m` shifts
/// every score by the same constant and the order survives, which is why this
/// could sit behind a suite that compares orderings. Jaccard is
/// `a / (m + w - a)`, where `m` is not an offset, so the ranking itself moves.
#[test]
fn a_dense_query_bit_in_the_padding_is_not_part_of_the_query() {
    const NARROW: u32 = 200;
    let dir = tempfile::tempdir().expect("tempdir");
    let idx = Index::create(
        YesnoStore::open(dir.path().join("db")).expect("open"),
        1,
        NARROW,
    )
    .expect("create");
    {
        let mut w = idx.writer();
        for i in 0..40u64 {
            let mut c = vec![0u64; 4];
            for k in 0..9u64 {
                let b = ((i * 17 + k * 23) % u64::from(NARROW)) as usize;
                c[b >> 6] |= 1u64 << (b & 63);
            }
            w.put(DocId(i), CodeRef::Dense(&c)).expect("put");
        }
        w.commit().expect("commit");
    }
    idx.refresh_stats().expect("stats");

    let mut clean = vec![0u64; 4];
    for b in [3usize, 40, 71, 130, 199] {
        clean[b >> 6] |= 1u64 << (b & 63);
    }
    // Bit 250 is inside the fourth word and at or above `dims`: padding.
    let mut padded = clean.clone();
    padded[250 >> 6] |= 1u64 << (250 & 63);

    for metric in [Metric::Jaccard, Metric::Cosine, Metric::Hamming] {
        for path in [PathHint::Gather, PathHint::DenseScan, PathHint::Inverted] {
            let run = |c: &[u64]| {
                idx.search()
                    .code(CodeRef::Dense(c))
                    .metric(metric)
                    .k(10)
                    .path(path)
                    .execute()
                    .expect("search")
                    .hits
            };
            assert_eq!(
                run(&padded),
                run(&clean),
                "{metric:?} {path:?}: a padding bit changed the answer"
            );
        }
    }
}
