//! Resuming after an eviction, and `explain()`.
//!
//! The resume path is the kind of code that is written once and never executed
//! again, so every test here checks that the fault **fired** as well as that the
//! result was right. A passing resume test with zero injections proves nothing.

use haiiie_core::{Consistency, DocId, Index, Metric, Path, PathHint, SetStore, YesnoStore};
use haiiie_testkit::{Corpus, EvictingStore, MemStore, Shape};

fn corpus() -> Corpus {
    // Several blocks, so an eviction lands mid-scan rather than before it.
    Corpus::generate(61, 32, 2 * 65_536 + 100, Shape::Balanced)
}

fn ingest(store: EvictingStore, c: &Corpus) -> Index<EvictingStore> {
    store.disarm();
    let idx = Index::create(store, 4, c.dims).expect("create");
    let mut w = idx.writer();
    for i in 0..c.len() {
        w.put(DocId(i as u64), c.code(i)).expect("put");
    }
    w.commit().expect("commit");
    idx
}

#[test]
fn a_resumed_scan_returns_the_same_answer() {
    let c = corpus();
    let reference = {
        let idx = Index::create(MemStore::new(), 4, c.dims).expect("create");
        let mut w = idx.writer();
        for i in 0..c.len() {
            w.put(DocId(i as u64), c.code(i)).expect("put");
        }
        w.commit().expect("commit");
        idx.search()
            .code(c.code(0))
            .metric(Metric::Hamming)
            .k(10)
            .path(PathHint::Inverted)
            .execute()
            .expect("reference")
            .hits
    };

    let idx = ingest(EvictingStore::new(7, 3), &c);
    idx.store().arm(3);
    let got = idx
        .search()
        .code(c.code(0))
        .metric(Metric::Hamming)
        .k(10)
        .path(PathHint::Inverted)
        .consistency(Consistency::ResumeOnEviction { max_retries: 8 })
        .execute()
        .expect("resumed search");

    assert!(
        idx.store().evictions() > 0,
        "no eviction was injected, so this test proves nothing"
    );
    assert_eq!(got.stats.resumes, idx.store().evictions() as u32);
    assert_eq!(got.hits, reference, "a resumed scan changed the answer");
}

/// A retry must redo the failed block, not continue inside it. If it resumed
/// mid-block the block's earlier documents would be counted twice, which sorts
/// and truncates into a plausible answer -- so this checks the count, not just
/// the top-k.
#[test]
fn a_retry_does_not_double_count_the_failed_block() {
    let c = corpus();
    let idx = ingest(EvictingStore::new(11, 4), &c);

    let clean = idx
        .search()
        .code(c.code(1))
        .metric(Metric::Dot)
        .k(usize::MAX)
        .path(PathHint::Inverted)
        .execute()
        .expect("clean");

    idx.store().arm(4);
    let resumed = idx
        .search()
        .code(c.code(1))
        .metric(Metric::Dot)
        .k(usize::MAX)
        .path(PathHint::Inverted)
        .consistency(Consistency::ResumeOnEviction { max_retries: 8 })
        .execute()
        .expect("resumed");

    assert!(idx.store().evictions() > 0, "no eviction injected");
    assert_eq!(resumed.scored, clean.scored, "a block was scored twice");
    assert_eq!(resumed.hits.len(), clean.hits.len());
    assert_eq!(resumed.hits, clean.hits);
}

#[test]
fn strict_consistency_surfaces_the_eviction() {
    let c = corpus();
    let idx = ingest(EvictingStore::new(5, 1), &c);
    idx.store().arm(1);
    let err = idx
        .search()
        .code(c.code(2))
        .metric(Metric::Dot)
        .k(5)
        .path(PathHint::Inverted)
        .consistency(Consistency::Strict)
        .execute()
        .expect_err("strict must not retry");
    assert!(err.is_snapshot_expired(), "wrong error: {err}");
}

#[test]
fn exhausting_the_retry_budget_surfaces_the_eviction() {
    let c = corpus();
    let idx = ingest(EvictingStore::new(3, 100), &c);
    idx.store().arm(100);
    let err = idx
        .search()
        .code(c.code(0))
        .metric(Metric::Dot)
        .k(5)
        .path(PathHint::Inverted)
        .consistency(Consistency::ResumeOnEviction { max_retries: 2 })
        .execute()
        .expect_err("the budget must run out");
    assert!(err.is_snapshot_expired(), "wrong error: {err}");
}

#[test]
fn the_forward_paths_resume_too() {
    let c = Corpus::generate(63, 32, 2 * 65_536, Shape::Balanced);
    // A forward scan reads one block per block; the inverted path reads one per
    // query dimension per block. The injection interval has to match the path
    // or the fault never fires -- which the `evictions() > 0` assertion below
    // caught when this was tuned for the inverted path.
    let idx = ingest(EvictingStore::new(2, 2), &c);
    let clean = idx
        .search()
        .code(c.code(0))
        .metric(Metric::Dot)
        .k(6)
        .path(PathHint::Gather)
        .execute()
        .expect("clean")
        .hits;
    idx.store().arm(2);
    let resumed = idx
        .search()
        .code(c.code(0))
        .metric(Metric::Dot)
        .k(6)
        .path(PathHint::Gather)
        .consistency(Consistency::ResumeOnEviction { max_retries: 8 })
        .execute()
        .expect("resumed");
    assert!(idx.store().evictions() > 0, "no eviction injected");
    assert_eq!(resumed.hits, clean);
}

#[test]
fn explain_reports_the_plan_without_scoring() {
    let c = Corpus::generate(67, 128, 500, Shape::Balanced);
    let idx = Index::create(MemStore::new(), 4, c.dims).expect("create");
    let mut w = idx.writer();
    for i in 0..c.len() {
        w.put(DocId(i as u64), c.code(i)).expect("put");
    }
    w.commit().expect("commit");

    let before = idx
        .search()
        .code(c.code(0))
        .metric(Metric::Jaccard)
        .k(7)
        .explain()
        .expect("explain");
    assert_eq!(before.path, Path::Inverted);
    assert_eq!(before.k, 7);
    assert_eq!(before.dims, 128);
    assert_eq!(before.blocks, 1);
    assert_eq!(
        before.blocks_with_stats, 0,
        "no statistics have been built yet"
    );
    assert!(before.exactness.contains("refinement"));
    assert!(
        before.path_reason.contains("whole-query placeholder")
            && before.path_reason.contains("per-block planner"),
        "explain must not present its placeholder as an executed path: {}",
        before.path_reason
    );

    idx.refresh_stats().expect("refresh");
    let after = idx
        .search()
        .code(c.code(0))
        .metric(Metric::Dot)
        .k(7)
        .explain()
        .expect("explain");
    assert_eq!(after.blocks_with_stats, 1);
    assert!(after.exactness.contains("exact"));
    // Renders without panicking and mentions the thing a reader came for.
    assert!(format!("{after}").contains("Inverted"));
}

/// Resuming after an eviction, over a store whose snapshots offer **cursors**.
///
/// Every other test in this file injects into `MemStore`, which returns `None`
/// from `open_lanes` — so no cursor exists under injection, and the invariants
/// belonging to a stateful cursor were unreachable from this suite. The one that
/// matters: a cursor is bound to the snapshot it was opened on, and a scan that
/// takes a fresh snapshot after an eviction must rebuild its cursors against it.
/// Deleting that rebuild left the entire workspace green.
///
/// This is the same shape upstream found in their own regression test the same
/// day — a fix with two call sites guarded at one. Here the second site was not
/// merely unguarded but *unreachable*, because the double could not produce the
/// state the invariant is about.
///
/// The forward cursor is deliberately in scope: it is opened lazily on the first
/// forward block, so a scan that resumes after touching one has both kinds of
/// cursor live across the retry.
///
/// What this test covers is that the cursor path is **reached** under injection
/// at all, which it was not before: the injector returned no cursor regardless
/// of what it wrapped. Whether the rebuild itself is correct is the next test's
/// job, and needed a change to the injector to be askable at all.
#[test]
fn a_resumed_scan_rebuilds_its_cursors_against_the_new_snapshot() {
    let dir = tempfile::tempdir().expect("tempdir");
    let c = Corpus::generate(31, 128, 200_000, Shape::Balanced);

    let store = EvictingStore::wrapping(
        YesnoStore::open(dir.path().join("db")).expect("open"),
        // Frequent enough to land inside a block's reads rather than between
        // blocks, which is where a stale cursor would still be consulted.
        23,
        0,
    );
    let idx = Index::create(store, 4, c.dims).expect("create");
    {
        let mut w = idx.writer();
        for i in 0..c.len() {
            w.put(DocId(i as u64), c.code(i)).expect("put");
            w.flush_if_large(2_000_000).expect("flush");
        }
        w.commit().expect("commit");
    }
    idx.store().inner().flush().expect("checkpoint");
    idx.refresh_stats().expect("stats");

    let truth = |path: PathHint| {
        idx.store().disarm();
        idx.search()
            .code(c.code(0))
            .metric(Metric::Hamming)
            .k(10)
            .path(path)
            .execute()
            .expect("reference")
            .hits
    };

    for path in [PathHint::Inverted, PathHint::Gather] {
        let want = truth(path);
        idx.store().arm(6);
        let got = idx
            .search()
            .code(c.code(0))
            .metric(Metric::Hamming)
            .k(10)
            .path(path)
            .consistency(Consistency::ResumeOnEviction { max_retries: 16 })
            .execute()
            .expect("resumed search");
        assert!(
            idx.store().evictions() > 0,
            "{path:?}: no eviction was injected, so this proves nothing"
        );
        assert!(got.stats.resumes > 0, "{path:?}: the scan never resumed");
        assert_eq!(
            got.hits, want,
            "{path:?}: results changed across an eviction that rebuilt cursors"
        );
    }
}

/// A cursor that survived a retry would read the **old version**, and here that
/// is visible.
///
/// # Why the previous test could not ask this
///
/// The injector simulates eviction by returning an error and leaves the
/// underlying snapshot valid, so a stale cursor and a rebuilt one read identical
/// bytes and no assertion about results can separate them. Deleting the rebuild
/// left the whole workspace green.
///
/// The missing ingredient was a **write landing between the fault and the
/// retry**. A writer thread racing the scan cannot be relied on to land in that
/// window; a callback at the injection point lands in it by construction, which
/// is what `EvictingStore::on_evict` is for.
///
/// # The shape of the observation
///
/// The hook gives one document every bit of the query. After the retry the scan
/// sees a version in which that document is the best possible match, so a
/// correctly rebuilt cursor ranks it first. A cursor carried over from the
/// evicted snapshot reads the posting lists as they were, where that document is
/// unremarkable, and it does not appear at all.
#[test]
fn a_cursor_carried_across_a_retry_would_read_the_old_version() {
    let dir = tempfile::tempdir().expect("tempdir");
    let c = Corpus::generate(41, 128, 200_000, Shape::Balanced);
    let dims = c.dims;

    // Two handles on one database: one for the scan to read through the
    // injector, one for the hook to write through. `YesnoStore` is a cheap
    // handle and all clones share the store, so this is one database.
    let raw = YesnoStore::open(dir.path().join("db")).expect("open");
    let writer_side = raw.clone();

    // **A fresh document per arm.** The hook promoting one id would make the
    // second arm unobservable: that promotion is already durable by then, so the
    // snapshot a stale cursor holds contains it too and nothing distinguishes a
    // rebuilt cursor from a carried-over one. Each fault promotes the next id,
    // and the test asks about whichever one this arm's fault produced.
    const PROMOTABLE: [u64; 2] = [150_001, 170_003];
    let query = c.codes[0].clone();
    let q = query.clone();
    let fired = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let promoted_now = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(u64::MAX));
    let (fired_h, promoted_h) = (fired.clone(), promoted_now.clone());

    // 60 reads in, which `arm` makes a count from the start of the search. Deep
    // enough into the first block that the forward cursor has been opened -- a
    // fault that arrives before it leaves nothing stale to carry across, so the
    // forward reset would go unguarded no matter how the test ends.
    let store = EvictingStore::wrapping(raw, 60, 0).on_evict(move || {
        // A second index over the same database. Built here rather than
        // captured because the first one does not exist yet when this closure
        // is made, and writing through the engine keeps every invariant the
        // suite checks -- the weight planes and the forward row move with the
        // posting lists.
        let n = fired_h.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let id = PROMOTABLE[n.min(PROMOTABLE.len() - 1)];
        let idx = Index::open(writer_side.clone(), 4).expect("open for write");
        let mut w = idx.writer();
        w.put(haiiie_core::DocId(id), haiiie_core::CodeRef::Dense(&q))
            .expect("promote");
        w.commit().expect("commit");
        promoted_h.store(id, std::sync::atomic::Ordering::SeqCst);
    });

    let idx = Index::create(store, 4, dims).expect("create");
    {
        let mut w = idx.writer();
        for i in 0..c.len() {
            w.put(DocId(i as u64), c.code(i)).expect("put");
            w.flush_if_large(2_000_000).expect("flush");
        }
        w.commit().expect("commit");
    }
    idx.store().inner().flush().expect("checkpoint");

    // **Both paths.** They hold different cursors and reset them in different
    // places: the inverted path reads the posting lists through the lanes opened
    // with the scan, the forward paths read the codes through a cursor opened
    // lazily on the first forward block. Covering only one leaves the other's
    // reset unguarded, which is how the serial rebuild went missing for a day.
    for (idx_of_arm, path) in [PathHint::Inverted, PathHint::Gather]
        .into_iter()
        .enumerate()
    {
        idx.store().disarm();
        let before = idx
            .search()
            .code(haiiie_core::CodeRef::Dense(&query))
            .metric(Metric::Hamming)
            .k(10)
            .path(path)
            .execute()
            .expect("before");
        let will_promote = PROMOTABLE[idx_of_arm];
        assert!(
            !before.hits.iter().any(|h| h.id.get() == will_promote),
            "{path:?}: {will_promote} is already in the top-k, so promoting it \
             proves nothing"
        );

        // Arm exactly one fault. The hook fires with it, so the retry's snapshot
        // contains a promotion the evicted one did not.
        idx.store().arm(1);
        let fired_before = idx.store().evictions();
        let after = idx
            .search()
            .code(haiiie_core::CodeRef::Dense(&query))
            .metric(Metric::Hamming)
            .k(10)
            .path(path)
            .consistency(Consistency::ResumeOnEviction { max_retries: 8 })
            .execute()
            .expect("resumed search");

        assert_eq!(
            idx.store().evictions(),
            fired_before + 1,
            "{path:?}: the fault did not fire"
        );
        assert!(after.stats.resumes > 0, "{path:?}: the scan never resumed");
        let id = promoted_now.load(std::sync::atomic::Ordering::SeqCst);
        assert_eq!(id, will_promote, "{path:?}: the hook promoted the wrong id");
        assert!(
            after.hits.iter().any(|h| h.id.get() == id),
            "{path:?}: document {id} was promoted during this scan's retry and is \
             absent, so the scan read from the snapshot it was told to abandon.\n  \
             got {:?}",
            after.hits.iter().map(|h| h.id.get()).collect::<Vec<_>>()
        );

        let promoted = after
            .hits
            .iter()
            .find(|h| h.id.get() == id)
            .expect("checked above");
        let m: u32 = query.iter().map(|w| w.count_ones()).sum();
        assert_eq!(promoted.inter, m, "{path:?}: not carrying every query bit");
        assert_eq!(
            promoted.score, after.hits[0].score,
            "{path:?}: the promoted document does not share the best score"
        );
        assert_eq!(
            after.hits[0].id.get(),
            0,
            "{path:?}: the id tie-break changed"
        );
    }
}

/// Admission cursors are a third snapshot-bound state, independent of scoring.
/// The second read fails after LIVE has been read but before ATTR is returned.
/// A write in that window changes membership, not merely a document's score.
#[test]
fn resumed_admission_sees_new_liveness_and_attributes() {
    use haiiie_core::{CodeRef, Filter};
    for insert_new in [false, true] {
        for path in [PathHint::Gather, PathHint::Inverted] {
            let dir = tempfile::tempdir().unwrap();
            let raw = YesnoStore::open(dir.path().join("db")).unwrap();
            let writer_side = raw.clone();
            let store = EvictingStore::wrapping(raw, 2, 0).on_evict(move || {
                let idx = Index::open(writer_side.clone(), 1).unwrap();
                let mut w = idx.writer();
                if insert_new {
                    w.put(DocId(1), CodeRef::Dense(&[15])).unwrap();
                }
                w.unattr(DocId(0), 7).unwrap();
                w.attr(DocId(1), 7).unwrap();
                w.commit().unwrap();
            });
            let idx = Index::create(store, 1, 8).unwrap();
            {
                let mut w = idx.writer();
                for id in [0, 65_536, 131_072] {
                    w.put(DocId(id), CodeRef::Dense(&[0])).unwrap();
                    w.attr(DocId(id), 7).unwrap();
                }
                if !insert_new {
                    w.put(DocId(1), CodeRef::Dense(&[15])).unwrap();
                }
                w.commit().unwrap();
            }
            idx.store().inner().flush().unwrap();
            let query = || {
                idx.search()
                    .code(CodeRef::Dense(&[15]))
                    .metric(Metric::Hamming)
                    .filter(Filter::Term(7))
                    .path(path)
                    .k(10)
            };
            let before = query().execute().unwrap();
            assert!(!before.hits.iter().any(|h| h.id == DocId(1)));
            idx.store().arm(1);
            let got = query()
                .consistency(Consistency::ResumeOnEviction { max_retries: 1 })
                .execute()
                .unwrap();
            assert_eq!(idx.store().evictions(), 1);
            assert_eq!(got.stats.resumes, 1);
            assert!(got.version_range.1 > got.version_range.0);
            let clean = query().execute().unwrap();
            assert_eq!(got.hits, clean.hits, "insert_new={insert_new}, {path:?}");
            assert_eq!(got.hits[0].id, DocId(1));
            assert!(!got.hits.iter().any(|h| h.id == DocId(0)));
        }
    }
}

/// A retry can turn a partially scored block into an empty one. It must discard
/// the interrupted hits even when the retry never enters a scoring kernel.
#[test]
fn an_empty_retry_discards_partial_forward_hits() {
    use haiiie_core::{CodeRef, Filter};
    for delete in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let raw = YesnoStore::open(dir.path().join("db")).unwrap();
        let writer_side = raw.clone();
        // LIVE, ATTR, first forward chunk, then failure on the second chunk.
        let store = EvictingStore::wrapping(raw, 4, 0).on_evict(move || {
            let idx = Index::open(writer_side.clone(), 1).unwrap();
            let mut w = idx.writer();
            for id in [0, 256] {
                if delete {
                    w.delete(DocId(id)).expect("delete");
                } else {
                    w.unattr(DocId(id), 7).unwrap();
                }
            }
            w.commit().unwrap();
        });
        let idx = Index::create(store, 1, 256).unwrap();
        {
            let mut w = idx.writer();
            for id in [0, 256, 65_536] {
                w.put(DocId(id), CodeRef::Dense(&[1, 0, 0, 0])).unwrap();
                w.attr(DocId(id), 7).unwrap();
            }
            w.commit().unwrap();
        }
        idx.store().inner().flush().unwrap();
        let query = || {
            idx.search()
                .code(CodeRef::Dense(&[1, 0, 0, 0]))
                .metric(Metric::Hamming)
                .filter(Filter::Term(7))
                .path(PathHint::Gather)
                .k(10)
        };
        idx.store().arm(1);
        let got = query().execute().unwrap();
        assert_eq!(idx.store().evictions(), 1);
        assert_eq!(got.stats.resumes, 1);
        assert_eq!(got.stats.blocks_skipped, 1);
        let clean = query().execute().unwrap();
        assert_eq!(
            got.hits, clean.hits,
            "delete={delete}: partial hits survived an empty retry"
        );
        assert_eq!(got.hits.len(), 1);
        assert_eq!(got.hits[0].id, DocId(65_536));
    }
}

/// A 512-bit row block holds 128 documents, so these IDs make one admission
/// block span three forward chunks. The third read fails after the first chunk
/// has already been scored; a retry that retained that partial work would
/// return a duplicate ID when k keeps every result.
#[test]
fn integer_row_scoring_discards_a_partially_read_block_before_retry() {
    use haiiie_core::{CodeRef, RowHit};

    let store = EvictingStore::new(3, 0);
    let idx = Index::create(store, 9, 512).expect("create");
    let documents = [
        (DocId(0), [1u64; 8]),
        (DocId(128), [2u64; 8]),
        (DocId(256), [3u64; 8]),
        (DocId(65_536), [4u64; 8]),
    ];
    {
        let mut writer = idx.writer();
        for (id, words) in &documents {
            writer.put(*id, CodeRef::Dense(words)).expect("put");
        }
        writer.commit().expect("commit");
    }

    let scorer = |row: &[u64]| row.iter().map(|&word| word as i64).sum();
    let clean = idx
        .row_search(&scorer)
        .k(usize::MAX)
        .execute()
        .expect("clean");
    assert_eq!(clean.scored, documents.len() as u64);

    idx.store().arm(1);
    let resumed = idx
        .row_search(&scorer)
        .k(usize::MAX)
        .consistency(Consistency::ResumeOnEviction { max_retries: 2 })
        .execute()
        .expect("resumed");
    assert_eq!(
        idx.store().evictions(),
        1,
        "the partial-block fault did not fire"
    );
    assert_eq!(resumed.stats.resumes, 1);
    assert_eq!(resumed.scored, clean.scored);
    assert_eq!(resumed.hits, clean.hits);
    assert_eq!(
        resumed.hits,
        vec![
            RowHit {
                id: DocId(65_536),
                score: 32,
            },
            RowHit {
                id: DocId(256),
                score: 24,
            },
            RowHit {
                id: DocId(128),
                score: 16,
            },
            RowHit {
                id: DocId(0),
                score: 8,
            },
        ]
    );
}

#[test]
fn parallel_integer_row_scoring_retries_the_whole_snapshot() {
    use haiiie_core::CodeRef;

    let dir = tempfile::tempdir().expect("tempdir");
    let store =
        EvictingStore::wrapping(YesnoStore::open(dir.path().join("db")).expect("open"), 8, 0);
    let idx = Index::create(store, 10, 512).expect("create");
    let zero = [0u64; 8];
    {
        let mut writer = idx.writer();
        for id in 0..32_768 {
            writer.put(DocId(id), CodeRef::Dense(&zero)).expect("put");
            writer.flush_if_large(2_000_000).expect("flush");
        }
        writer.commit().expect("commit");
    }
    idx.store().inner().flush().expect("checkpoint");

    let scorer = |row: &[u64]| row.iter().map(|word| i64::from(word.count_ones())).sum();
    let clean = idx
        .row_search(&scorer)
        .k(10)
        .execute()
        .expect("clean serial row search");

    idx.store().arm(1);
    let resumed = idx
        .row_search(&scorer)
        .k(10)
        .threads(8)
        .consistency(Consistency::ResumeOnEviction { max_retries: 2 })
        .execute()
        .expect("parallel retry");

    assert_eq!(
        idx.store().evictions(),
        1,
        "the parallel snapshot fault did not fire exactly once"
    );
    assert_eq!(resumed.stats.resumes, 1);
    assert_eq!(resumed.scored, clean.scored);
    assert_eq!(resumed.hits, clean.hits);
    assert_eq!(resumed.version_range.0, resumed.version_range.1);
}

/// A write between finding the last block and scoring must not let the
/// reported version claim a complete scan of a newer, longer index.
#[test]
fn scan_keeps_the_snapshot_that_supplied_its_last_block() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use haiiie_core::store::{Batch, Version};
    use haiiie_core::{CodeRef, Result};
    use haiiie_testkit::memstore::MemSnapshot;

    #[derive(Clone)]
    struct GrowAfterSnapshot {
        inner: Arc<MemStore>,
        armed: Arc<AtomicBool>,
    }

    impl SetStore for GrowAfterSnapshot {
        type Snap = MemSnapshot;

        fn snapshot(&self) -> Result<Self::Snap> {
            let old = self.inner.snapshot()?;
            if self.armed.swap(false, Ordering::AcqRel) {
                let index = Index::open(self.clone(), 4)?;
                let mut writer = index.writer();
                writer.put(DocId(65_536), CodeRef::Dense(&[u64::MAX]))?;
                writer.commit()?;
            }
            Ok(old)
        }

        fn write(&self, batch: &Batch) -> Result<Version> {
            self.inner.write(batch)
        }
    }

    let store = GrowAfterSnapshot {
        inner: Arc::new(MemStore::new()),
        armed: Arc::new(AtomicBool::new(false)),
    };
    let index = Index::create(store.clone(), 4, 64).unwrap();
    let mut writer = index.writer();
    writer.put(DocId(0), CodeRef::Dense(&[1])).unwrap();
    let old_version = writer.commit().unwrap();
    store.armed.store(true, Ordering::Release);
    let query = [u64::MAX];
    let result = index
        .search()
        .code(CodeRef::Dense(&query))
        .k(2)
        .path(PathHint::Inverted)
        .execute()
        .unwrap();
    assert!(
        !store.armed.load(Ordering::Acquire),
        "growth hook never ran"
    );
    assert_eq!(result.version_range, (old_version, old_version));
    assert_eq!(
        result.hits.iter().map(|hit| hit.id).collect::<Vec<_>>(),
        vec![DocId(0)]
    );
    let newer = index
        .search()
        .code(CodeRef::Dense(&query))
        .k(2)
        .path(PathHint::Inverted)
        .execute()
        .unwrap();
    assert_eq!(newer.hits.first().unwrap().id, DocId(65_536));
}
