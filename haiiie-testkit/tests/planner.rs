//! The planner's path choice, asserted without a stopwatch.
//!
//! `Hits::path` reports the path the scan actually took, so the rule can be
//! checked as a function of its inputs rather than by timing two runs and hoping
//! the difference clears the noise. A timing test of a planner is a flaky test
//! of a planner.
//!
//! # What is being pinned
//!
//! The crossover between the inverted and forward paths **moves with the query's
//! width**, because only one side of the comparison depends on it: gather reads
//! whole rows and costs the same for any query, while the inverted path reads
//! one posting list per query bit. Measured at D=256 over 262 144 documents, the
//! filter at which the inverted path stops winning runs from one in sixteen for
//! a 118-bit query to one in five hundred for a 15-bit one.
//!
//! A single constant served the dense end and made the planner up to **2.4x**
//! slower than the alternative for thin queries, across the whole band from one
//! in twenty-four to one in five hundred. Sparse codes are an advertised use
//! case, so that band is not a corner.
//!
//! These assertions are about *which* path, not how fast.
//!
//! # Watched to fail, by accident
//!
//! This header said "not sabotage-checked" for about an hour, on the grounds
//! that no single constant *could* satisfy both assertions below -- an argument,
//! not a measurement. It has since been observed: a sabotage script collapsing
//! `crossover_for` to the old single constant was interrupted before it
//! restored the file, and the next gate run failed here, on the thin-query
//! assertion, with `left: Gather, right: Inverted`.
//!
//! So the claim is earned for that case, and the manner of earning it is the
//! joke: the check that found the damage was this test, and the damage was a
//! sabotage of the very rule it exists to pin. The remaining direction --
//! collapsing to the *thin* constant, which should break the dense assertion --
//! is still argument rather than measurement.
//!
//! Running it is cheap: make `crossover_for` ignore its arguments and return
//! either constant, then run this file.

use haiiie_core::{CodeRef, DocId, Filter, Index, Metric, Path, PathHint, SetStore, YesnoStore};
use haiiie_testkit::{Corpus, Shape};

/// Keep every `n`th set bit, so query width varies without changing the corpus.
fn thinned(code: &[u64], keep_every: usize) -> Vec<u64> {
    let mut out = vec![0u64; code.len()];
    let mut seen = 0usize;
    for (w, o) in code.iter().zip(out.iter_mut()) {
        let mut bits = *w;
        while bits != 0 {
            let b = bits.trailing_zeros();
            bits &= bits - 1;
            if seen.is_multiple_of(keep_every) {
                *o |= 1u64 << b;
            }
            seen += 1;
        }
    }
    out
}

#[test]
fn the_path_choice_moves_with_the_query_width() {
    let dir = tempfile::tempdir().expect("tempdir");
    let dims = 256u32;
    let docs = 131_072usize;
    let c = Corpus::generate(5, dims, docs, Shape::Balanced);
    let idx = Index::create(
        YesnoStore::open(dir.path().join("db")).expect("open"),
        1,
        dims,
    )
    .expect("create");
    {
        let mut w = idx.writer();
        for i in 0..c.len() {
            w.put(DocId(i as u64), c.code(i)).expect("put");
            // One in 64: inside the band where the two regimes disagree.
            if i % 64 == 0 {
                w.attr(DocId(i as u64), 1).expect("attr");
            }
            w.flush_if_large(8_000_000).expect("flush");
        }
        w.commit().expect("commit");
    }
    idx.store().flush().expect("checkpoint");
    idx.refresh_stats().expect("stats");

    let taken = |q: &[u64], filter: Filter| {
        idx.search()
            .code(CodeRef::Dense(q))
            .metric(Metric::Hamming)
            .k(10)
            .filter(filter)
            .path(PathHint::Auto)
            .execute()
            .expect("search")
            .path
    };

    let dense = c.codes[0].clone();
    let thin = thinned(&dense, 8);
    let (m_dense, m_thin): (u32, u32) = (
        dense.iter().map(|w| w.count_ones()).sum(),
        thin.iter().map(|w| w.count_ones()).sum(),
    );
    assert!(
        f64::from(m_dense) / f64::from(dims) >= 0.40,
        "the dense query is not dense: {m_dense} of {dims}"
    );
    assert!(
        f64::from(m_thin) / f64::from(dims) < 0.10,
        "the thin query is not thin: {m_thin} of {dims}"
    );

    // **The same filter, opposite choices.** This is the whole point: one
    // constant cannot produce both, so a regression to one collapses this.
    assert_eq!(
        taken(&dense, Filter::Term(1)),
        Path::Gather,
        "a {m_dense}-bit query at one in 64 should take the forward path"
    );
    assert_eq!(
        taken(&thin, Filter::Term(1)),
        Path::Inverted,
        "a {m_thin}-bit query at one in 64 should stay on the inverted path; \
         measured, the forward path is about 1.9x slower there"
    );

    // Wide-query crossover measured on the current tiled/heap kernels: one
    // in 8 stays inverted, while one in 12 gathers. Explicit IDs preserve
    // the scattered admission pattern without changing the persisted fixture.
    for (stride, expected) in [(8usize, Path::Inverted), (12, Path::Gather)] {
        let ids = (0..docs as u64).step_by(stride).map(DocId).collect();
        assert_eq!(
            taken(&dense, Filter::Ids(ids)),
            expected,
            "the {m_dense}-bit query chose the wrong path at one in {stride}"
        );
    }
    // The 90-bit query lies between the measured 60-79-bit and 114-157-bit
    // groups. It must not inherit the new wide-query multiplier at one in 12.
    let middle = [u64::MAX, (1u64 << 26) - 1, 0, 0];
    let ids = (0..docs as u64).step_by(12).map(DocId).collect();
    assert_eq!(taken(&middle, Filter::Ids(ids)), Path::Inverted);

    // Unfiltered, both stay inverted: the forward path is linear in the admitted
    // set and there is nothing to admit less of.
    assert_eq!(taken(&dense, Filter::All), Path::Inverted);
    assert_eq!(taken(&thin, Filter::All), Path::Inverted);
}

/// Cursor setup is observable work, not a timing threshold. Alternate sparse
/// and dense filter blocks to force Auto to enter both paths in one scan, and
/// check the same contract when the store declines cursors entirely.
#[test]
fn scoring_cursors_open_only_for_paths_that_run() {
    use haiiie_core::{
        Batch, Result, SetSnapshot,
        slice::BlockMask,
        store::{Lanes, Version},
    };
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    struct Watched {
        raw: YesnoStore,
        opens: Arc<[AtomicUsize; 3]>,
        addressed: Arc<AtomicUsize>,
        decline: bool,
    }
    struct Snapshot {
        raw: <YesnoStore as SetStore>::Snap,
        opens: Arc<[AtomicUsize; 3]>,
        addressed: Arc<AtomicUsize>,
        decline: bool,
    }
    impl SetStore for Watched {
        type Snap = Snapshot;
        fn snapshot(&self) -> Result<Snapshot> {
            Ok(Snapshot {
                raw: self.raw.snapshot()?,
                opens: self.opens.clone(),
                addressed: self.addressed.clone(),
                decline: self.decline,
            })
        }
        fn write(&self, b: &Batch) -> Result<Version> {
            self.raw.write(b)
        }
    }
    impl SetSnapshot for Snapshot {
        fn version(&self) -> Version {
            self.raw.version()
        }
        fn load(&self, k: u64) -> Result<Vec<u64>> {
            self.raw.load(k)
        }
        fn key_range(&self, lo: u64, hi: u64) -> Result<Vec<u64>> {
            self.raw.key_range(lo, hi)
        }
        fn cardinality(&self, k: u64) -> Result<u64> {
            self.raw.cardinality(k)
        }
        fn contains(&self, k: u64, o: u64) -> Result<bool> {
            self.raw.contains(k, o)
        }
        fn max(&self, k: u64) -> Result<Option<u64>> {
            self.raw.max(k)
        }
        fn load_block(&self, k: u64, b: u64, out: &mut BlockMask) -> Result<bool> {
            self.addressed.fetch_add(1, Ordering::Relaxed);
            self.raw.load_block(k, b, out)
        }
        fn open_lanes(&self, keys: &[u64]) -> Result<Option<Box<dyn Lanes>>> {
            match keys.first().map(|k| (k >> 20) & 255) {
                Some(0x10 | 0x11) => {
                    self.opens[0].fetch_add(1, Ordering::Relaxed);
                }
                Some(0x20) => {
                    self.opens[1].fetch_add(1, Ordering::Relaxed);
                }
                Some(0x02) => {
                    self.opens[2].fetch_add(1, Ordering::Relaxed);
                }
                _ => {}
            }
            if self.decline {
                Ok(None)
            } else {
                self.raw.open_lanes(keys)
            }
        }
    }
    let dir = tempfile::tempdir().unwrap();
    {
        let idx = Index::create(YesnoStore::open(dir.path()).unwrap(), 1, 8).unwrap();
        let mut w = idx.writer();
        for block in 0..6 {
            for row in 0..32 {
                let id = DocId(block * 65_536 + row);
                w.put(id, CodeRef::Dense(&[(row ^ 3) & 255])).unwrap();
                if row == 0 || block % 2 == 1 {
                    w.attr(id, 1).unwrap();
                }
                if row == 0 {
                    w.attr(id, 2).unwrap();
                }
            }
        }
        w.commit().unwrap();
        idx.store().flush().unwrap();
    }
    let raw = Index::open(YesnoStore::open(dir.path()).unwrap(), 1).unwrap();
    for decline in [false, true] {
        let opens = Arc::new([
            AtomicUsize::new(0),
            AtomicUsize::new(0),
            AtomicUsize::new(0),
        ]);
        let addressed = Arc::new(AtomicUsize::new(0));
        let idx = Index::open(
            Watched {
                raw: raw.store().clone(),
                opens: opens.clone(),
                addressed: addressed.clone(),
                decline,
            },
            1,
        )
        .unwrap();
        // Expectations are [inverted, forward]. D=8, |Q|=4: one in 32
        // selects Gather, while all 32 candidates select Inverted.
        for (filter, auto_needed) in [
            (Filter::None, [false, false]),
            (Filter::Term(2), [false, true]),
            (Filter::Term(1), [true, true]),
            (Filter::All, [true, false]),
        ] {
            let want = raw
                .search()
                .code(CodeRef::Dense(&[15]))
                .metric(Metric::Hamming)
                .filter(filter.clone())
                .path(PathHint::Inverted)
                .k(10)
                .execute()
                .unwrap();
            for path in [
                PathHint::Auto,
                PathHint::Gather,
                PathHint::DenseScan,
                PathHint::Inverted,
            ] {
                let needed = if matches!(filter, Filter::None) {
                    [false, false]
                } else {
                    match path {
                        PathHint::Auto => auto_needed,
                        PathHint::Inverted => [true, false],
                        _ => [false, true],
                    }
                };
                for threads in [1, 4] {
                    for n in opens.iter() {
                        n.store(0, Ordering::Relaxed);
                    }
                    let got = idx
                        .search()
                        .code(CodeRef::Dense(&[15]))
                        .metric(Metric::Hamming)
                        .filter(filter.clone())
                        .path(path)
                        .threads(threads)
                        .k(10)
                        .execute()
                        .unwrap();
                    assert_eq!(got.hits, want.hits);
                    for (i, needed) in needed.into_iter().enumerate() {
                        let n = opens[i].load(Ordering::Relaxed);
                        assert_eq!(
                            n > 0,
                            needed,
                            "decline={decline} path={path:?} filter={filter:?} family={i}"
                        );
                        assert!(
                            n <= threads,
                            "a cursor open was retried per block: {n} for {threads} workers"
                        );
                    }
                }
            }
        }

        // `count` and `explain` are separate walks from scoring. Each should
        // negotiate its admission cursor once, and an offered cursor should
        // remove all per-block addressed reads. A declining store retains the
        // fallback and the same answer.
        for helper in ["count", "explain"] {
            for n in opens.iter() {
                n.store(0, Ordering::Relaxed);
            }
            addressed.store(0, Ordering::Relaxed);
            match helper {
                "count" => assert_eq!(idx.search().filter(Filter::Term(1)).count().unwrap(), 99),
                "explain" => assert_eq!(idx.search().explain().unwrap().blocks, 6),
                _ => unreachable!(),
            }
            assert_eq!(opens[2].load(Ordering::Relaxed), 1, "helper={helper}");
            assert_eq!(
                addressed.load(Ordering::Relaxed) > 0,
                decline,
                "helper={helper}: addressed reads must be only the declined-cursor fallback"
            );
        }
    }
}
