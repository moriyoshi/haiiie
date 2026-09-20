//! Allocation budgets for the inverted scan.
//!
//! Its own binary, single-threaded, because the counter is process-wide.

use haiiie_core::{DocId, Index, Kernel, Metric, PathHint};
use haiiie_testkit::counting_alloc::{self, Counting};
use haiiie_testkit::{Corpus, MemStore, Shape};

#[global_allocator]
static ALLOC: Counting = Counting;

fn build(dims: u32, docs: usize) -> (Index<MemStore>, Corpus) {
    let c = Corpus::generate(9_001, dims, docs, Shape::Balanced);
    let idx = Index::create(MemStore::new(), 5, dims).expect("create");
    let mut w = idx.writer();
    for i in 0..c.len() {
        w.put(DocId(i as u64), c.code(i)).expect("put");
    }
    w.commit().expect("commit");
    (idx, c)
}

/// Reading more query dimensions must not allocate proportionally more.
///
/// # What this targets, and what it deliberately does not
///
/// Each query dimension is read into **one reused scratch plane** and folded
/// into **one reused accumulator**, so widening the query multiplies the *work*
/// and must not multiply the *allocations*. That is the structure M3 built and
/// the one that decays silently, since a per-dimension buffer would return
/// identical answers.
///
/// An earlier version of this test varied the corpus size instead and failed at
/// 4.4 million allocations against 450 -- correctly, but for an unrelated
/// reason it then misattributed to the accumulator. The scan materializes the
/// live set and the admitted set as `BTreeSet`s, which is `O(N)` allocations per
/// query in the *filter* layer, and no arrangement of corpus sizes can separate
/// that from the per-block term because more blocks always means more
/// documents. Tracked separately as `live-set-is-materialized-per-query`; the
/// lesson kept here is that a budget must vary the axis it is accusing.
#[test]
fn widening_the_query_does_not_allocate_proportionally_more() {
    let _serial = counting_alloc::lock();
    let (idx, _) = build(512, 2_000);

    let run = |bits: u32| {
        let mut q = vec![0u64; 8];
        for b in 0..bits {
            q[(b as usize) >> 6] |= 1u64 << (b & 63);
        }
        let (_, n) = counting_alloc::count(|| {
            idx.search()
                .code(haiiie_core::CodeRef::Dense(&q))
                .metric(Metric::Hamming)
                .k(10)
                .path(PathHint::Inverted)
                .kernel(Kernel::CarrySave)
                .execute()
                .expect("search")
        });
        n
    };
    let narrow = run(8);
    let wide = run(256);

    // The threshold is placed between two **measured** values, not under the
    // healthy one. Healthy: 173 054 against 173 059, a difference of **5** for
    // 32x the dimensions read. Sabotaged with one scratch plane allocated per
    // dimension: +248, tracking the dimension count exactly.
    //
    // A ratio bound of `wide < narrow * 4` was the first attempt and it is the
    // weak-test failure mode: it permits a difference of 519 000, so it would
    // have passed the sabotage while looking like a guard. 64 sits between 5
    // and 248 with room for incidental variation, and the property being
    // defended is that the difference is **constant**, which an absolute slack
    // states and a ratio does not.
    assert!(
        wide <= narrow + 64,
        "allocations grew {narrow} -> {wide} over 8 -> 256 query dimensions; \
         a per-dimension buffer or a per-fold allocation has crept in"
    );
}

/// Both accumulators must obey the same budget: a carry-save kernel that
/// allocated its intermediates would still agree with ripple on every answer.
#[test]
fn the_carry_save_kernel_does_not_allocate_more_than_ripple() {
    let _serial = counting_alloc::lock();
    let (idx, c) = build(256, 200);
    let run = |k: Kernel| {
        let (_, n) = counting_alloc::count(|| {
            idx.search()
                .code(c.code(1))
                .metric(Metric::Hamming)
                .k(10)
                .path(PathHint::Inverted)
                .kernel(k)
                .execute()
                .expect("search")
        });
        n
    };
    let ripple = run(Kernel::Ripple);
    let csa = run(Kernel::CarrySave);
    // Measured: both 8 881, identical. The slack of 4 is for incidental
    // variation, not headroom -- a per-block allocation in `finish_into` or a
    // per-dimension one in `push_at` moves this by hundreds, and did when
    // sabotaged. This is the assertion that actually caught that sabotage.
    assert!(
        csa <= ripple + 4,
        "carry-save allocated {csa} against ripple's {ripple}; the accumulator \
         or its finish step is allocating per block"
    );
}

/// Repeating the same query must cost the same allocations every time. A cache
/// that grew, or a buffer that was reallocated rather than reused, shows here
/// and nowhere else.
#[test]
fn repeating_a_query_costs_the_same_every_time() {
    let _serial = counting_alloc::lock();
    let (idx, c) = build(128, 500);
    let once = |_: usize| {
        let (_, n) = counting_alloc::count(|| {
            idx.search()
                .code(c.code(2))
                .metric(Metric::Dot)
                .k(5)
                .path(PathHint::Inverted)
                .execute()
                .expect("search")
        });
        n
    };
    let first = once(0);
    for i in 1..4 {
        assert_eq!(
            once(i),
            first,
            "run {i} allocated differently from the first"
        );
    }
}

/// Allocations must not scale with the corpus.
///
/// # The budget this replaces, and why it could not be written before
///
/// An M3 attempt at this measured 450 allocations at 20 documents against
/// **4 377 015** at 196 628, and mis-attributed the growth to the accumulator.
/// The cause was the filter layer: `live` and `admitted` were built as
/// `BTreeSet`s per query, which is `O(N)` allocations and dwarfed everything the
/// kernels did. M5 made the filter Block-aligned -- one reused mask per operand,
/// nothing per document -- so the property is finally assertable rather than
/// merely desirable.
///
/// The bound is placed between measured values, per the rule this suite keeps
/// relearning: growing the corpus 10 000x must not grow allocations by more than
/// a small factor, where the old behaviour grew them by 9 700x.
#[test]
fn allocations_do_not_scale_with_the_corpus() {
    let _serial = counting_alloc::lock();

    let run = |docs: usize| {
        let (idx, c) = build(32, docs);
        let (_, n) = counting_alloc::count(|| {
            idx.search()
                .code(c.code(0))
                .metric(Metric::Hamming)
                .k(10)
                .path(PathHint::Inverted)
                .execute()
                .expect("search")
        });
        n
    };
    let small = run(20);
    let large = run(20 + 2 * 65_536);
    // Measured: **13 allocations at 20 documents, 16 at 131 092** -- a whole
    // query path that is constant in the corpus, which is the property. With
    // the deep-cloning snapshot restored the pair is 237 and 1 485 256, so the
    // bound is placed between 16 and 1.5 million rather than under either.
    //
    // An absolute slack, not a ratio: at these magnitudes `small * 8` would
    // permit 104 and read as a guard while tolerating a twentyfold regression.
    assert!(
        large <= small + 16,
        "allocations grew {small} -> {large} over 20 -> 131 092 documents; \
         something in the query path is proportional to the corpus again"
    );
}

/// `Search::count` is documented as "one non-materializing cardinality", and
/// nothing pinned it.
///
/// That phrase is a performance claim of exactly the kind this crate has broken
/// before: the scan once built the live and admitted sets as `BTreeSet`s, which
/// was 4.4 million allocations at 196 000 documents and made the inverted path
/// economical only at small sizes. `count` walks the same filter by a **separate
/// path** -- evaluate each block, popcount, discard -- so it could regress on
/// its own without touching the scan.
///
/// Measured after admission cursors: **1 allocation at 20 documents and 1 at
/// 131 092**. The fixed allocation is the cursor's key list; the two block masks
/// remain stack arrays. The budget is written as a small absolute slack rather
/// than one so that an unrelated allocation in `snapshot` does not fail the gate,
/// while any per-block or per-document allocation does.
///
/// The search below is the **control**. A zero from a counter that has stopped
/// counting looks exactly like a zero from a path that does not allocate, and
/// this is the one test here whose expected value is zero.
#[test]
fn counting_does_not_materialize_anything() {
    let _serial = counting_alloc::lock();

    let run = |docs: usize| {
        let (idx, c) = build(32, docs);
        let (_, counted) = counting_alloc::count(|| {
            idx.search()
                .filter(haiiie_core::Filter::All)
                .count()
                .expect("count")
        });
        let (_, searched) = counting_alloc::count(|| {
            idx.search()
                .code(c.code(0))
                .metric(Metric::Hamming)
                .k(10)
                .path(PathHint::Inverted)
                .execute()
                .expect("search")
        });
        (counted, searched)
    };

    let (small, small_ctl) = run(20);
    let (large, large_ctl) = run(20 + 2 * 65_536);

    assert!(
        small_ctl > 0 && large_ctl > 0,
        "the allocation counter reported nothing for a search either, so the \
         zeros below are not evidence about `count`"
    );
    assert!(
        small <= 4 && large <= 4,
        "count allocated {small} at 20 documents and {large} at 131 092; it is \
         documented as non-materializing and measured at zero for both"
    );
}

/// Persisted lanes must not add one owned mask allocation per query dimension.
/// `MemStore` has no lane cursor, so its width guard cannot exercise this path.
///
/// On a flushed YesnoStore with 512 dimensions and 2,000 balanced documents,
/// seed 9,001, a warm counted Hamming query allocated 673 times at eight query
/// bits and 2,918 at 256 ( difference 2,245 ). The roughly nine allocations
/// per added lane come from opening each upstream key stream and are present
/// in CSA too. Temporarily allocating one extra mask on every tiled lane made
/// the pair 691 and 3,184 ( difference 2,493 ) and failed this 2,300 bound.
/// It therefore guards the tiled-only regression without raising any existing
/// allocation budget. Both the default and explicitly chosen tiled kernel run.
#[test]
fn yesno_tiled_width_does_not_allocate_a_mask_per_lane() {
    let _serial = counting_alloc::lock();
    let dir = tempfile::tempdir().expect("temporary store");
    let corpus = Corpus::generate(9_001, 512, 2_000, Shape::Balanced);
    let idx = Index::create(
        haiiie_core::YesnoStore::open(dir.path()).expect("open"),
        5,
        512,
    )
    .expect("create");
    let mut writer = idx.writer();
    for i in 0..corpus.len() {
        writer.put(DocId(i as u64), corpus.code(i)).expect("put");
    }
    writer.commit().expect("commit");
    use haiiie_core::SetStore;
    idx.store().flush().expect("flush");

    let run = |bits: u32, kernel: Option<Kernel>| {
        let mut q = [0u64; 8];
        for b in 0..bits {
            q[(b as usize) >> 6] |= 1u64 << (b & 63);
        }
        let search = || {
            let mut query = idx
                .search()
                .code(haiiie_core::CodeRef::Dense(&q))
                .metric(Metric::Hamming)
                .k(10)
                .path(PathHint::Inverted);
            if let Some(kernel) = kernel {
                query = query.kernel(kernel);
            }
            query.execute().expect("search")
        };
        search(); // Warm storage and query code before the counted run.
        let (_, count) = counting_alloc::count(search);
        count
    };
    for kernel in [None, Some(Kernel::Tiled)] {
        let narrow = run(8, kernel);
        let wide = run(256, kernel);
        assert!(
            wide <= narrow + 2_300,
            "YesnoStore tiled {kernel:?} allocated {narrow} -> {wide} across 8 -> 256 query bits; check upstream per-key stream planning as well as per-lane mask expansion"
        );
    }
}
