//! The edges of a forward row: its last bit, and its neighbour's first.
//!
//! # Why this file exists
//!
//! A document's forward row is `row_bits` consecutive ordinals starting at
//! `base`, and clearing it used to be a loop over `0..row_bits` -- a shape with
//! no boundary to get wrong. It is now a single range operation with an
//! **inclusive** upper bound, because the storage layer's range form costs one
//! WAL record and one copy-on-write clone for the whole span instead of one per
//! ordinal. That is a real saving and it introduces a real off-by-one.
//!
//! Sabotage says nothing else tests it. Moving either end of that range by one,
//! in either direction, left the whole suite green: the existing corpora never
//! set the last bit of a row, and no test checks a document adjacent to a
//! deleted one. Both faults are silent -- a stale bit inflates a weight, and an
//! over-long range clears a bit belonging to the next document -- and the
//! documents involved keep answering queries.
//!
//! # `row_bits`, not `dims`
//!
//! The row is `row_bits` wide, padded up to a multiple of 64, and bits at or
//! above `dims` are never written. So a test that wants the **last bit of the
//! row** to be meaningful needs `dims` to be a multiple of 64; otherwise the
//! last bit is padding, is masked off at ingest, and cannot witness anything.
//!
//! The residual tile case also crosses the native 65,536-ordinal chunk edge:
//! it compares packed writes with the point oracle after reopen, then exercises
//! overwrite and delete/readd where packing must decline.

use haiiie_core::{
    CodeRef, DocId, Index, KeySpace, Metric, PathHint, SetSnapshot, SetStore, YesnoStore,
};

/// A multiple of 64, so `row_bits == dims` and the row's last bit is a real
/// dimension rather than padding.
const DIMS: u32 = 128;
const WORDS: usize = 2;

fn code(bits: &[u32]) -> Vec<u64> {
    let mut v = vec![0u64; WORDS];
    for &b in bits {
        v[(b as usize) >> 6] |= 1u64 << (b & 63);
    }
    v
}

fn index() -> (tempfile::TempDir, Index<YesnoStore>) {
    let dir = tempfile::tempdir().expect("tempdir");
    let idx = Index::create(
        YesnoStore::open(dir.path().join("db")).expect("open"),
        1,
        DIMS,
    )
    .expect("create");
    (dir, idx)
}

/// Every document's `(intersection, weight)` against a query, by id.
fn weights(idx: &Index<YesnoStore>, q: &[u64]) -> Vec<(u64, u32, u32)> {
    let mut v: Vec<(u64, u32, u32)> = idx
        .search()
        .code(CodeRef::Dense(q))
        .metric(Metric::Hamming)
        .k(32)
        .path(PathHint::Gather)
        .execute()
        .expect("search")
        .hits
        .iter()
        .map(|h| (h.id.get(), h.inter, h.weight))
        .collect();
    v.sort_unstable();
    v
}

#[test]
fn deleting_a_document_leaves_the_next_row_untouched() {
    let (_d, idx) = index();
    // Every document carries bit 0, which is the **first ordinal of its row** --
    // exactly the bit a clearing range that runs one past its own row would
    // take from its neighbour.
    {
        let mut w = idx.writer();
        for i in 0..4u64 {
            w.put(DocId(i), CodeRef::Dense(&code(&[0, 5, 9])))
                .expect("put");
        }
        w.commit().expect("commit");
    }
    idx.refresh_stats().expect("stats");

    {
        let mut w = idx.writer();
        w.delete(DocId(1)).expect("delete");
        w.commit().expect("commit");
    }
    idx.refresh_stats().expect("stats");

    let q = code(&[0, 5, 9]);
    assert_eq!(
        weights(&idx, &q),
        vec![(0, 3, 3), (2, 3, 3), (3, 3, 3)],
        "deleting document 1 changed a neighbour's row"
    );
}

#[test]
fn a_replacement_clears_the_last_bit_of_the_row() {
    let (_d, idx) = index();
    let last = DIMS - 1;
    {
        let mut w = idx.writer();
        // Document 0 holds the row's final bit; document 1 is the neighbour
        // whose first bit an over-long range would reach.
        w.put(DocId(0), CodeRef::Dense(&code(&[1, last])))
            .expect("put");
        w.put(DocId(1), CodeRef::Dense(&code(&[0, 2])))
            .expect("put");
        w.commit().expect("commit");
    }

    // A second put of document 0 **in a new batch** takes the differencing
    // path; the within-batch re-put below takes the blind-clearing one. Both
    // clear the row, and only one of them was reachable from the other tests.
    {
        let mut w = idx.writer();
        w.put(DocId(0), CodeRef::Dense(&code(&[1]))).expect("put");
        w.commit().expect("commit");
    }
    idx.refresh_stats().expect("stats");
    assert_eq!(
        weights(&idx, &code(&[1])),
        vec![(0, 1, 1), (1, 0, 2)],
        "the row's last bit survived a differencing replacement"
    );

    {
        let mut w = idx.writer();
        w.put(DocId(0), CodeRef::Dense(&code(&[1, last])))
            .expect("put");
        // Same id, same batch: the old row lives only in the batch, so this
        // falls back to clearing the whole row blindly.
        w.put(DocId(0), CodeRef::Dense(&code(&[1]))).expect("put");
        w.commit().expect("commit");
    }
    idx.refresh_stats().expect("stats");
    assert_eq!(
        weights(&idx, &code(&[1])),
        vec![(0, 1, 1), (1, 0, 2)],
        "the row's last bit survived a blind-clearing replacement"
    );
}

/// A deleted id can be added again, and holds exactly its new code.
///
/// # Why this is the test that catches a short clear
///
/// A `delete` that leaves one bit of the forward row set is invisible while the
/// document stays deleted -- it is out of the live set, so nothing scores it,
/// and the design never reuses an ordinal on its own. But **a caller may put
/// the same id again**, and then `is_present` reads the live set, sees nothing,
/// and takes the fresh-insert path, which clears nothing because it has no
/// reason to. The stale bit joins the new code and the document answers with a
/// row it was never given.
///
/// So the clear on the delete path is the only thing standing between a
/// re-added document and a corrupted row, and this is the operation that
/// observes it. Delete-then-re-add is otherwise untested.
#[test]
fn a_deleted_id_added_again_holds_only_its_new_code() {
    let (_d, idx) = index();
    let last = DIMS - 1;
    {
        let mut w = idx.writer();
        // The original code sets the row's final bit and several others.
        w.put(DocId(0), CodeRef::Dense(&code(&[0, 3, 64, last])))
            .expect("put");
        w.put(DocId(1), CodeRef::Dense(&code(&[3]))).expect("put");
        w.commit().expect("commit");
    }
    {
        let mut w = idx.writer();
        w.delete(DocId(0)).expect("delete");
        w.commit().expect("commit");
    }
    {
        let mut w = idx.writer();
        // Re-added with a code sharing nothing with the old one.
        w.put(DocId(0), CodeRef::Dense(&code(&[3]))).expect("put");
        w.commit().expect("commit");
    }
    idx.refresh_stats().expect("stats");

    assert_eq!(
        weights(&idx, &code(&[3])),
        vec![(0, 1, 1), (1, 1, 1)],
        "a re-added document inherited bits from the code it used to hold"
    );
}

/// A pending delete invalidates the snapshot used for overwrite differencing.
/// Re-adding the same persisted code in one batch must rewrite every bit.
#[test]
fn a_same_batch_delete_then_readd_rewrites_the_persisted_binary_row() {
    let (_dir, index) = index();
    let original = code(&[0, 3, 64, DIMS - 1]);
    {
        let mut writer = index.writer();
        writer
            .put(DocId(0), CodeRef::Dense(&original))
            .expect("initial put");
        writer.commit().expect("commit");
    }
    {
        let mut writer = index.writer();
        writer.delete(DocId(0)).expect("delete");
        writer
            .put(DocId(0), CodeRef::Dense(&original))
            .expect("same-batch readd");
        writer.commit().expect("commit");
    }
    index.refresh_stats().expect("statistics");
    assert_eq!(weights(&index, &original), vec![(0, 4, 4)]);
}

/// A flush commits the new row, so a cached pre-flush row cannot be used for
/// the next difference even while the writer and its live mask stay in place.
#[test]
fn a_flush_discards_the_cached_forward_row_before_the_next_overwrite() {
    let (_dir, index) = index();
    {
        let mut writer = index.writer();
        writer
            .put(DocId(0), CodeRef::Dense(&code(&[0])))
            .expect("initial put");
        writer.commit().expect("commit");
    }
    {
        let mut writer = index.writer();
        writer
            .put(DocId(0), CodeRef::Dense(&code(&[1])))
            .expect("first overwrite");
        assert!(writer.flush_if_large(1).expect("flush").is_some());
        writer
            .put(DocId(0), CodeRef::Dense(&code(&[2])))
            .expect("second overwrite");
        writer.commit().expect("commit");
    }
    assert_eq!(weights(&index, &code(&[2])), vec![(0, 1, 1)]);
}

#[test]
fn packed_residual_tiles_match_point_writes_after_reopen_and_churn() {
    let fast_dir = tempfile::tempdir().expect("fast tempdir");
    let slow_dir = tempfile::tempdir().expect("slow tempdir");
    let model_id = [7u8; 32];
    let fast = Index::create_with_model_id(
        YesnoStore::open(fast_dir.path().join("db")).expect("fast open"),
        1,
        512,
        model_id,
    )
    .expect("fast create");
    let slow = Index::create_with_model_id(
        YesnoStore::open(slow_dir.path().join("db")).expect("slow open"),
        1,
        512,
        model_id,
    )
    .expect("slow create");
    let rows: Vec<_> = (0..256u64)
        .map(|id| {
            let mut code = [0u64; 8];
            if id % 17 != 0 {
                code[0] |= 1;
                code[7] |= 1u64 << 63;
                code[(id % 8) as usize] |= 1u64 << ((id * 13) % 64);
            }
            (DocId(id), code)
        })
        .collect();
    let replacement = [0xaaaa_5555_aaaa_5555u64; 8];
    let readded = [0x1234_5678_9abc_def0u64; 8];

    {
        let mut writer = fast.writer();
        assert!(
            writer
                .try_put_residual_tile(&rows[..128])
                .expect("tile one")
        );
        for &(id, _) in &rows[..128] {
            writer.attr(id, (id.get() % 3) as u32).expect("attr");
        }
        writer.flush_if_large(0).expect("tile flush");
        assert!(
            writer
                .try_put_residual_tile(&rows[128..])
                .expect("tile two")
        );
        for &(id, _) in &rows[128..] {
            writer.attr(id, (id.get() % 3) as u32).expect("attr");
        }
        assert!(
            !writer
                .try_put_residual_tile(&rows[..128])
                .expect("existing tile")
        );
        writer
            .put(DocId(0), CodeRef::Dense(&replacement))
            .expect("replace");
        writer.delete(DocId(129)).expect("delete");
        writer
            .put(DocId(129), CodeRef::Dense(&readded))
            .expect("readd");
        writer.commit().expect("fast commit");
    }
    {
        let mut writer = slow.writer();
        for &(id, code) in &rows {
            writer.put(id, CodeRef::Dense(&code)).expect("slow put");
            writer.attr(id, (id.get() % 3) as u32).expect("attr");
        }
        writer
            .put(DocId(0), CodeRef::Dense(&replacement))
            .expect("replace");
        writer.delete(DocId(129)).expect("delete");
        writer
            .put(DocId(129), CodeRef::Dense(&readded))
            .expect("readd");
        writer.commit().expect("slow commit");
    }
    fast.store().flush().expect("fast checkpoint");
    slow.store().flush().expect("slow checkpoint");
    drop(fast);
    drop(slow);
    let fast = Index::open(
        YesnoStore::open(fast_dir.path().join("db")).expect("fast reopen"),
        1,
    )
    .expect("fast index");
    let slow = Index::open(
        YesnoStore::open(slow_dir.path().join("db")).expect("slow reopen"),
        1,
    )
    .expect("slow index");
    let keys = KeySpace::new(1);
    let fast_snap = fast.store().snapshot().expect("fast snapshot");
    let slow_snap = slow.store().snapshot().expect("slow snapshot");
    for key in [
        keys.forward(),
        keys.live(),
        keys.attr(0),
        keys.attr(1),
        keys.attr(2),
    ] {
        assert_eq!(
            fast_snap.load(key).expect("fast set"),
            slow_snap.load(key).expect("slow set"),
            "key {key}"
        );
    }
}
