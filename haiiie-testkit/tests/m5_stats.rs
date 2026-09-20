//! Per-block statistics: they must tighten the bound and must never change an
//! answer.
//!
//! A bound that is too tight drops results silently -- no crash, no count
//! change, just a document missing. So every test here checks the answer
//! against the same search with statistics absent, and against the oracle.

use haiiie_core::{BlockStats, DocId, Index, Metric, PathHint, SetSnapshot, SetStore};
use haiiie_testkit::{Corpus, MemStore, Shape, oracle};

const RATIO: [Metric; 2] = [Metric::Jaccard, Metric::Cosine];

/// Weight-sorted ordinals, which is what makes a block's weight range narrow.
/// The caller assigns ordinals, so this is a caller strategy rather than an
/// index feature -- and it is the one the measurement says matters.
fn ingest_weight_sorted(c: &Corpus) -> (Index<MemStore>, Vec<usize>) {
    let mut order: Vec<usize> = (0..c.len()).collect();
    order.sort_by_key(|&i| c.codes[i].iter().map(|w| w.count_ones()).sum::<u32>());
    let idx = Index::create(MemStore::new(), 2, c.dims).expect("create");
    let mut w = idx.writer();
    for (ord, &i) in order.iter().enumerate() {
        w.put(DocId(ord as u64), c.code(i)).expect("put");
    }
    w.commit().expect("commit");
    (idx, order)
}

#[test]
fn statistics_never_change_an_answer() {
    for shape in [
        Shape::Balanced,
        Shape::Sparse,
        Shape::Degenerate,
        Shape::OrdinalRuns,
    ] {
        let c = Corpus::generate(31, 128, 300, shape);
        let (idx, order) = ingest_weight_sorted(&c);

        // A reordered corpus, so the oracle indexes match the assigned ordinals.
        let reordered = Corpus {
            dims: c.dims,
            codes: order.iter().map(|&i| c.codes[i].clone()).collect(),
        };

        for metric in RATIO {
            let before = idx
                .search()
                .code(c.code(0))
                .metric(metric)
                .k(8)
                .path(PathHint::Inverted)
                .execute()
                .expect("before");
            idx.refresh_stats().expect("refresh");
            let after = idx
                .search()
                .code(c.code(0))
                .metric(metric)
                .k(8)
                .path(PathHint::Inverted)
                .execute()
                .expect("after");

            let want = oracle::top_k(&reordered, c.code(0), metric, 8, None, None);
            assert_eq!(before.hits, want, "{shape:?}/{metric:?} without statistics");
            assert_eq!(after.hits, want, "{shape:?}/{metric:?} with statistics");
        }
    }
}

/// The statistics must actually narrow, or they are inert and every test above
/// is passing for the wrong reason.
#[test]
fn statistics_tighten_the_ratio_bound() {
    let c = Corpus::generate(37, 128, 2_000, Shape::Balanced);

    for metric in RATIO {
        // A **fresh index per metric**. Sharing one across the loop meant the
        // first metric's `refresh_stats` had already run before the second
        // metric's "loose" measurement, so the second compared statistics
        // against statistics and reported no change. The bound was correct
        // throughout; the test was measuring a state it believed it had set up.
        let (idx, _) = ingest_weight_sorted(&c);
        let loose = idx
            .search()
            .code(c.code(0))
            .metric(metric)
            .k(10)
            .path(PathHint::Inverted)
            .execute()
            .expect("loose")
            .scored;
        idx.refresh_stats().expect("refresh");
        let tight = idx
            .search()
            .code(c.code(0))
            .metric(metric)
            .k(10)
            .path(PathHint::Inverted)
            .execute()
            .expect("tight")
            .scored;
        assert!(
            tight < loose,
            "{metric:?}: statistics did not narrow anything ({loose} -> {tight})"
        );
    }
}

/// A write must invalidate its block's statistics, in the same commit.
///
/// This is the property that makes a stale bound impossible rather than
/// unlikely. Without it a document could be inserted with a weight below the
/// recorded `w_min`, and the bound would then exclude it from its own result.
#[test]
fn a_write_invalidates_the_blocks_statistics() {
    let c = Corpus::generate(41, 64, 50, Shape::Balanced);
    let (idx, _) = ingest_weight_sorted(&c);
    idx.refresh_stats().expect("refresh");

    let key = idx.keys().stat(0);
    let snap = idx.store().snapshot().expect("snap");
    assert!(
        BlockStats::from_ordinals(&snap.load(key).expect("load")).is_some(),
        "refresh_stats wrote nothing, so the invalidation test proves nothing"
    );

    let mut w = idx.writer();
    w.put(DocId(0), c.code(1)).expect("put");
    w.commit().expect("commit");

    let snap = idx.store().snapshot().expect("snap");
    assert!(
        BlockStats::from_ordinals(&snap.load(key).expect("load")).is_none(),
        "the block kept its statistics across a write to it"
    );
}

#[test]
fn absent_and_repeated_deletes_do_not_invalidate_live_block_statistics() {
    let c = Corpus::generate(43, 64, 50, Shape::Balanced);
    let (idx, _) = ingest_weight_sorted(&c);
    idx.refresh_stats().expect("refresh");
    let key = idx.keys().stat(0);
    let before = idx
        .store()
        .snapshot()
        .expect("snapshot")
        .load(key)
        .expect("load");
    assert!(BlockStats::from_ordinals(&before).is_some());

    let mut writer = idx.writer();
    writer.delete(DocId(5_000)).expect("absent delete");
    assert_eq!(writer.pending(), 0, "an absent ID staged operations");
    writer.commit().expect("commit no-op");
    let after = idx
        .store()
        .snapshot()
        .expect("snapshot")
        .load(key)
        .expect("load");
    assert_eq!(after, before, "an absent delete cleared valid statistics");

    let mut writer = idx.writer();
    writer.delete(DocId(0)).expect("live delete");
    let pending = writer.pending();
    assert!(pending > 0);
    writer.delete(DocId(0)).expect("repeated delete");
    assert_eq!(
        writer.pending(),
        pending,
        "a repeated delete staged operations"
    );
    writer.commit().expect("commit live delete");
    assert!(
        BlockStats::from_ordinals(
            &idx.store()
                .snapshot()
                .expect("snapshot")
                .load(key)
                .expect("load"),
        )
        .is_none(),
        "a live delete must still invalidate statistics"
    );
}

#[test]
fn stats_round_trip_and_refuse_nonsense() {
    for (w_min, w_max) in [(0u32, 0u32), (0, 128), (7, 7), (1, 4096)] {
        let s = BlockStats { w_min, w_max };
        assert_eq!(BlockStats::from_ordinals(&s.to_ordinals()), Some(s));
    }
    // Absent, and inconsistent, are both refused rather than guessed.
    assert_eq!(BlockStats::from_ordinals(&[]), None);
    assert_eq!(
        BlockStats::from_ordinals(&[1, 2]),
        None,
        "no presence marker"
    );
    let bad = BlockStats { w_min: 9, w_max: 1 };
    assert_eq!(
        BlockStats::from_ordinals(&bad.to_ordinals()),
        None,
        "w_min > w_max"
    );
}

/// `explain()` reports the block weight spread, and the number tracks the thing
/// it claims to diagnose.
///
/// The spread exists so a caller querying by Jaccard or cosine on arbitrarily
/// ordered ids can **see** what it is costing them, rather than read about it in
/// prose they may never reach. A diagnostic that does not move when the
/// condition it diagnoses changes is worse than no diagnostic.
///
/// # The threshold is the one the tool publishes, not one invented here
///
/// This first asserted `tight * 4 < loose` and failed at 13 against 47 -- a
/// real 3.6x separation, rejected by a multiplier picked out of the air. The
/// property was sound and the number was a guess, which is the failure this
/// suite exists to catch in production code and is no better in a test.
///
/// It now asserts what `Plan`'s own rendering asserts: a spread above `dims / 4`
/// is reported to the user as "ids are not in weight order". So the test pins
/// **the advice the tool gives** -- the warning must fire for arbitrary ids and
/// stay silent for weight-sorted ones -- which is the thing that would actually
/// be wrong if this regressed.
#[test]
fn explain_reports_a_weight_spread_that_tracks_ordinal_order() {
    let c = Corpus::generate(11, 128, 131_072, Shape::Balanced);

    let (sorted, _) = ingest_weight_sorted(&c);
    sorted.refresh_stats().expect("stats");

    // The same codes, ids assigned in generation order.
    let arbitrary = Index::create(MemStore::new(), 2, c.dims).expect("create");
    {
        let mut w = arbitrary.writer();
        for i in 0..c.len() {
            w.put(DocId(i as u64), c.code(i)).expect("put");
        }
        w.commit().expect("commit");
    }
    arbitrary.refresh_stats().expect("stats");

    let plan = |idx: &Index<MemStore>| {
        idx.search()
            .code(c.code(0))
            .metric(Metric::Jaccard)
            .k(10)
            .explain()
            .expect("explain")
    };
    let (tight, loose) = (plan(&sorted), plan(&arbitrary));
    let (ts, ls) = (
        tight.block_weight_spread.expect("statistics"),
        loose.block_weight_spread.expect("statistics"),
    );

    assert!(
        ls * 4 > c.dims,
        "arbitrary ids report spread {ls} of {} bits, which the tool would call \
         weight-ordered",
        c.dims
    );
    assert!(
        ts * 4 <= c.dims,
        "weight-sorted ids report spread {ts} of {} bits, which the tool would \
         warn about",
        c.dims
    );
    assert!(
        loose.to_string().contains("not in weight order"),
        "no warning for arbitrary ids:\n{loose}"
    );
    assert!(
        !tight.to_string().contains("not in weight order"),
        "warned about weight-sorted ids:\n{tight}"
    );

    // And the spread must actually predict the pruning, or it is a number that
    // looks like evidence and is not.
    let scored = |idx: &Index<MemStore>| {
        idx.search()
            .code(c.code(0))
            .metric(Metric::Jaccard)
            .k(10)
            .path(PathHint::Inverted)
            .execute()
            .expect("search")
            .scored
    };
    assert!(
        scored(&sorted) < scored(&arbitrary),
        "narrower spread ({ts} against {ls}) did not score fewer documents: \
         {} against {}",
        scored(&sorted),
        scored(&arbitrary)
    );

    // No statistics at all: absent rather than zero, because zero is a
    // legitimate spread ( every document the same weight ) and would read as the
    // best possible case rather than as no information.
    let fresh = Index::create(MemStore::new(), 2, c.dims).expect("create");
    {
        let mut w = fresh.writer();
        w.put(DocId(0), c.code(0)).expect("put");
        w.commit().expect("commit");
    }
    assert_eq!(plan(&fresh).block_weight_spread, None);
}
