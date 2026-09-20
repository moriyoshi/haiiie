//! A parallel scan must return exactly what a serial one returns.
//!
//! The merge argument says it will -- the top-k of a union is the top-k of the
//! parts' top-k's, and blocks are independent -- but that is an argument, and
//! the failure it would miss is a total order that is not quite total. Two hits
//! with equal scores and no tie-break would come back in whatever order the
//! threads finished, which is a result that is correct on average and different
//! every run.

use haiiie_core::{DocId, Filter, Index, Metric, PathHint, YesnoStore};
use haiiie_testkit::{Corpus, Shape};

/// One linear metric and one ratio metric: those are the two scoring paths, and
/// a third of either adds runtime without adding a code path. The four-metric
/// sweep lives in the correctness suites, which run on a corpus small enough to
/// afford it -- this one needs five blocks before the parallel path engages at
/// all, and `MemStore` reads are slow enough that the matrix is the cost.
const METRICS: [Metric; 2] = [Metric::Hamming, Metric::Jaccard];

/// Several blocks, so the work actually divides -- built once and shared.
///
/// Five blocks is the smallest corpus the parallel path will take, so it cannot
/// be shrunk without disabling the thing under test. Ingesting it per test cost
/// 105 s; once, it costs 25 s.
/// Deliberately `YesnoStore` rather than `MemStore`. `MemStore` range-scans a
/// `BTreeSet` per block read, so ingesting five blocks into it took 60 s and the
/// suite ran for 80; the real store does the same work in a fraction of that and
/// is the path that ships. A double is the right subject when the question is
/// "do two implementations agree"; here the question is "does one implementation
/// depend on thread count", and the double only slows that down.
type Store = YesnoStore;

fn shared() -> &'static (tempfile::TempDir, Index<Store>, Corpus) {
    static CORPUS: std::sync::OnceLock<(tempfile::TempDir, Index<Store>, Corpus)> =
        std::sync::OnceLock::new();
    CORPUS.get_or_init(build)
}

fn build() -> (tempfile::TempDir, Index<Store>, Corpus) {
    let dir = tempfile::tempdir().expect("tempdir");
    let c = Corpus::generate(97, 128, 5 * 65_536 + 77, Shape::Balanced);
    let store = YesnoStore::open(dir.path().join("db")).expect("open");
    let idx = Index::create(store, 8, c.dims).expect("create");
    let mut w = idx.writer();
    for i in 0..c.len() {
        w.put(DocId(i as u64), c.code(i)).expect("put");
        if i % 3 == 0 {
            w.attr(DocId(i as u64), 1).expect("attr");
        }
        w.flush_if_large(4_000_000).expect("flush");
    }
    w.commit().expect("commit");
    idx.refresh_stats().expect("stats");
    (dir, idx, c)
}

#[test]
fn every_thread_count_returns_byte_identical_results() {
    let (_dir, idx, c) = shared();
    for metric in METRICS {
        for k in [1usize, 25] {
            let serial = idx
                .search()
                .code(c.code(0))
                .metric(metric)
                .k(k)
                .execute()
                .expect("serial");
            for threads in [2usize, 20] {
                let par = idx
                    .search()
                    .code(c.code(0))
                    .metric(metric)
                    .k(k)
                    .threads(threads)
                    .execute()
                    .expect("parallel");
                assert_eq!(
                    par.hits, serial.hits,
                    "{metric:?}/k={k}/{threads} threads disagreed with serial"
                );
                // **This asserted `par.scored == serial.scored` until 2026-09-19,
                // and a deliberate change made that false rather than a bug
                // making it fail.**
                //
                // The serial scan now carries the `k`-th best score found so
                // far into the next block's refinement, which prunes documents
                // a local threshold could not -- 126 160 exact scores down to
                // 67 672 at 524 288 documents. That threshold depends on the
                // order blocks are visited in, so work is no longer a function
                // of the query alone. The parallel path steals blocks and does
                // not carry one, so it prunes less.
                //
                // The plan designed for exactly this: a shared threshold raised
                // by CAS-max, where "a worker reading a stale lower tau prunes
                // less and is still exact". Deterministic **results**, and work
                // that depends on scheduling. Work-identity was true only while
                // no order-dependent pruning existed, and no amount of sharing
                // the threshold would restore it.
                //
                // So the equality is replaced by the invariant that survives and
                // still has teeth: **parallel may not do less work than serial**,
                // because it prunes with a weaker threshold or none. A parallel
                // scan that scored fewer documents would mean it had skipped
                // something, which is the failure this assertion exists to
                // catch. The results equality above is untouched and is the
                // property the project actually promises.
                assert!(
                    par.scored >= serial.scored,
                    "{metric:?}/k={k}/{threads} threads scored {} against serial's {}: \
                     parallel prunes with a weaker threshold, so it cannot do less work",
                    par.scored,
                    serial.scored
                );
            }
        }
    }
}

/// Repeated runs at the same thread count must also agree. A race that depends
/// on scheduling shows here and not in a single comparison against serial.
#[test]
fn repeated_parallel_runs_agree_with_each_other() {
    let (_dir, idx, c) = shared();
    let once = || {
        idx.search()
            .code(c.code(3))
            .metric(Metric::Hamming)
            .k(25)
            .threads(20)
            .execute()
            .expect("parallel")
            .hits
    };
    let first = once();
    for i in 1..4 {
        assert_eq!(once(), first, "run {i} differed from the first");
    }
}

#[test]
fn a_filter_composes_the_same_way_in_parallel() {
    let (_dir, idx, c) = shared();
    for metric in METRICS {
        let serial = idx
            .search()
            .code(c.code(1))
            .metric(metric)
            .k(12)
            .filter(Filter::Term(1))
            .execute()
            .expect("serial");
        let par = idx
            .search()
            .code(c.code(1))
            .metric(metric)
            .k(12)
            .filter(Filter::Term(1))
            .threads(8)
            .execute()
            .expect("parallel");
        assert_eq!(par.hits, serial.hits, "{metric:?}");
        assert_eq!(par.stats.blocks_visited, serial.stats.blocks_visited);
    }
}

/// Forcing each path must also be thread-count independent: the two scan
/// routines are different code and the planner picks between them per block.
#[test]
fn every_forced_path_is_thread_count_independent() {
    let (_dir, idx, c) = shared();
    for hint in [PathHint::Gather, PathHint::Inverted] {
        let serial = idx
            .search()
            .code(c.code(2))
            .metric(Metric::Hamming)
            .k(9)
            .path(hint)
            .execute()
            .expect("serial");
        let par = idx
            .search()
            .code(c.code(2))
            .metric(Metric::Hamming)
            .k(9)
            .path(hint)
            .threads(16)
            .execute()
            .expect("parallel");
        assert_eq!(par.hits, serial.hits, "{hint:?}");
    }
}

/// Asking for more threads than there are blocks must not spawn idle workers
/// or change the answer.
#[test]
fn more_threads_than_blocks_is_harmless() {
    let dir = tempfile::tempdir().expect("tempdir");
    let c = Corpus::generate(101, 64, 200, Shape::Balanced);
    let store = YesnoStore::open(dir.path().join("db")).expect("open");
    let idx = Index::create(store, 8, c.dims).expect("create");
    let mut w = idx.writer();
    for i in 0..c.len() {
        w.put(DocId(i as u64), c.code(i)).expect("put");
    }
    w.commit().expect("commit");

    let serial = idx.search().code(c.code(0)).k(5).execute().expect("serial");
    let par = idx
        .search()
        .code(c.code(0))
        .k(5)
        .threads(64)
        .execute()
        .expect("parallel");
    assert_eq!(par.hits, serial.hits);
}

fn opaque_integer_score(row: &[u64]) -> i64 {
    row.iter()
        .enumerate()
        .map(|(word, value)| i64::from(value.count_ones()) * (word as i64 + 1))
        .sum()
}

#[test]
fn opaque_integer_rows_use_several_workers_without_changing_the_answer() {
    let (_dir, idx, _c) = shared();
    let serial = idx
        .row_search(&opaque_integer_score)
        .k(25)
        .execute()
        .expect("serial row scoring");

    let seen = std::sync::Mutex::new(std::collections::HashSet::new());
    let scorer = |row: &[u64]| {
        seen.lock()
            .expect("thread recorder")
            .insert(std::thread::current().id());
        opaque_integer_score(row)
    };
    let parallel = idx
        .row_search(&scorer)
        .k(25)
        .threads(20)
        .execute()
        .expect("parallel row scoring");

    assert_eq!(parallel.hits, serial.hits);
    assert_eq!(parallel.scored, serial.scored);
    assert_eq!(parallel.stats.blocks_visited, serial.stats.blocks_visited);
    assert_eq!(parallel.stats.blocks_skipped, serial.stats.blocks_skipped);
    assert!(
        seen.into_inner().expect("thread recorder").len() > 1,
        "the corpus exceeds the measured parallel threshold, so more than one worker must score it"
    );
}
