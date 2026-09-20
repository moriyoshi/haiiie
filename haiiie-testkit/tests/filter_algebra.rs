//! Filter composition: `And`, `Or`, `Not`, `Ids` and `IdRange`.
//!
//! # Why this file exists
//!
//! Instrumenting every arm of the filter evaluator against the whole suite found
//! **`And`, `Or` and `IdRange` never evaluated once**, with `Ids` and `Not` at a
//! single evaluation each. `Term` and `All` carried 913 of the 921. The
//! composition operators -- the part of the filter language a caller actually
//! composes, and the part the gRPC layer maps its `and` / `or` / `not` clauses
//! onto -- had no coverage at all.
//!
//! A wrong answer here is a wrong *result set*, with no error and nothing for a
//! differential suite to catch: the oracle is handed the same `Filter` value and
//! agrees with whatever it means.
//!
//! # The oracle is written from the semantics, not from the evaluator
//!
//! `expect` below interprets a `Filter` directly over a `HashSet` of live ids.
//! It is the slow obvious implementation of what the documentation says a filter
//! means, which is the only kind of oracle worth having here -- one derived from
//! the evaluator would agree with its bugs.
//!
//! # Three semantics that a reader would reasonably guess wrong
//!
//! `IdRange(lo, hi)` is **half-open**: `hi` is excluded. `And` of no clauses is
//! everything live, `Or` of no clauses is nothing -- the identity of each
//! operation, which is right and is the opposite of what "an empty filter
//! matches nothing" would suggest for `And`. And `Not` complements **within the
//! live set**, not over the ordinal space, because an unbounded complement spans
//! 2^64 and is meaningful for a count rather than a candidate list.

use std::collections::{HashMap, HashSet};

use haiiie_core::{CodeRef, DocId, Filter, Index, Metric, PathHint, YesnoStore};

const DIMS: u32 = 128;
const WORDS: usize = 2;

/// Ids straddling the 65 536-ordinal block boundary in both directions, so the
/// per-block arithmetic in `Ids` and `IdRange` is exercised rather than assumed.
const IDS: &[u64] = &[0, 1, 2, 3, 65_534, 65_535, 65_536, 65_537, 131_072, 131_073];
const DELETED: u64 = 3;

fn code(bits: &[u32]) -> Vec<u64> {
    let mut v = vec![0u64; WORDS];
    for &b in bits {
        v[(b as usize) >> 6] |= 1u64 << (b & 63);
    }
    v
}

/// What each id's attributes are. Term 1 on multiples of three, term 2 on evens.
fn terms_of(id: u64) -> Vec<u32> {
    let mut v = Vec::new();
    if id.is_multiple_of(3) {
        v.push(1);
    }
    if id.is_multiple_of(2) {
        v.push(2);
    }
    v
}

/// The documented meaning of a filter, over the live set. Not derived from the
/// evaluator under test.
fn expect(f: &Filter, live: &HashSet<u64>, terms: &HashMap<u32, HashSet<u64>>) -> HashSet<u64> {
    match f {
        Filter::All => live.clone(),
        Filter::None => HashSet::new(),
        Filter::Term(t) => terms
            .get(t)
            .map_or_else(HashSet::new, |s| s.intersection(live).copied().collect()),
        Filter::Ids(ids) => ids
            .iter()
            .map(|d| d.get())
            .filter(|o| live.contains(o))
            .collect(),
        Filter::IdRange(lo, hi) => live.iter().copied().filter(|o| o >= lo && o < hi).collect(),
        Filter::And(cs) => cs.iter().fold(live.clone(), |acc, c| {
            acc.intersection(&expect(c, live, terms)).copied().collect()
        }),
        Filter::Or(cs) => cs.iter().fold(HashSet::new(), |acc, c| {
            acc.union(&expect(c, live, terms)).copied().collect()
        }),
        Filter::Not(c) => live.difference(&expect(c, live, terms)).copied().collect(),
        _ => panic!("a filter variant this oracle does not model"),
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    idx: Index<YesnoStore>,
    live: HashSet<u64>,
    terms: HashMap<u32, HashSet<u64>>,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().expect("tempdir");
    let idx = Index::create(
        YesnoStore::open(dir.path().join("db")).expect("open"),
        1,
        DIMS,
    )
    .expect("create");
    let mut terms: HashMap<u32, HashSet<u64>> = HashMap::new();
    {
        let mut w = idx.writer();
        for (i, &id) in IDS.iter().enumerate() {
            w.put(DocId(id), CodeRef::Dense(&code(&[0, 1, (i as u32) % 64])))
                .expect("put");
            for t in terms_of(id) {
                w.attr(DocId(id), t).expect("attr");
                terms.entry(t).or_default().insert(id);
            }
        }
        // A deleted document must never survive any filter, including `Not`.
        w.delete(DocId(DELETED)).expect("delete");
        w.commit().expect("commit");
    }
    idx.refresh_stats().expect("stats");
    let live: HashSet<u64> = IDS.iter().copied().filter(|&o| o != DELETED).collect();
    Fixture {
        _dir: dir,
        idx,
        live,
        terms,
    }
}

impl Fixture {
    /// Every id the engine admits under `f`, over all three forced paths.
    fn admitted(&self, f: &Filter) -> HashSet<u64> {
        let q = code(&[0, 1]);
        let mut agreed: Option<HashSet<u64>> = None;
        for path in [PathHint::Gather, PathHint::DenseScan, PathHint::Inverted] {
            let got: HashSet<u64> = self
                .idx
                .search()
                .code(CodeRef::Dense(&q))
                .metric(Metric::Hamming)
                .k(IDS.len())
                .filter(f.clone())
                .path(path)
                .execute()
                .expect("search")
                .hits
                .iter()
                .map(|h| h.id.get())
                .collect();
            if let Some(prev) = &agreed {
                assert_eq!(prev, &got, "{path:?} disagrees with another path on {f:?}");
            }
            agreed = Some(got);
        }
        agreed.expect("at least one path")
    }

    fn check(&self, f: Filter) {
        let want = expect(&f, &self.live, &self.terms);
        let got = self.admitted(&f);
        assert_eq!(got, want, "filter {f:?}");
        assert!(!got.contains(&DELETED), "a deleted document survived {f:?}");

        // `Search::count` is a **separate walk** over the same filter -- it
        // evaluates each block and popcounts rather than scoring -- so it can
        // disagree with the scan, and it had exactly one shape tested anywhere:
        // a single `Term`, over the wire. Every composition operator reaches it
        // for free from here.
        let counted = self.idx.search().filter(f.clone()).count().expect("count");
        assert_eq!(
            counted,
            want.len() as u64,
            "count disagrees with the admitted set for {f:?}"
        );
    }
}

#[test]
fn and_or_and_not_mean_intersection_union_and_complement() {
    let fx = fixture();
    let t1 = || Filter::Term(1);
    let t2 = || Filter::Term(2);

    fx.check(Filter::And(vec![t1(), t2()]));
    fx.check(Filter::Or(vec![t1(), t2()]));
    fx.check(Filter::Not(Box::new(t1())));
    fx.check(Filter::Not(Box::new(Filter::Not(Box::new(t1())))));
    fx.check(Filter::And(vec![t1(), Filter::Not(Box::new(t2()))]));
    fx.check(Filter::Or(vec![
        Filter::And(vec![t1(), t2()]),
        Filter::Not(Box::new(Filter::Or(vec![t1(), t2()]))),
    ]));
}

#[test]
fn an_empty_and_is_everything_and_an_empty_or_is_nothing() {
    let fx = fixture();
    // The identity of each operation. An implementation that treated both as
    // "matches nothing" would look defensible and would be wrong about `And`.
    fx.check(Filter::And(vec![]));
    fx.check(Filter::Or(vec![]));
    assert_eq!(fx.admitted(&Filter::And(vec![])), fx.live);
    assert!(fx.admitted(&Filter::Or(vec![])).is_empty());
}

#[test]
fn an_id_range_is_half_open_and_crosses_block_boundaries() {
    let fx = fixture();
    // Exactly the boundary: 65 536 is the first ordinal of block 1.
    fx.check(Filter::IdRange(0, 65_536));
    fx.check(Filter::IdRange(65_536, 131_072));
    fx.check(Filter::IdRange(65_535, 65_537));
    fx.check(Filter::IdRange(0, u64::MAX));
    fx.check(Filter::IdRange(5, 5));

    // Stated directly, because "inclusive" is the reasonable wrong guess.
    let upto = fx.admitted(&Filter::IdRange(0, 65_536));
    assert!(upto.contains(&65_535), "lo..hi must include hi - 1");
    assert!(!upto.contains(&65_536), "lo..hi must exclude hi");
}

#[test]
fn an_id_list_ignores_ids_that_are_absent_or_deleted() {
    let fx = fixture();
    fx.check(Filter::Ids(vec![DocId(0), DocId(65_536)]));
    // An id in no block of this index, an id that was deleted, and a duplicate.
    fx.check(Filter::Ids(vec![
        DocId(0),
        DocId(999_999),
        DocId(DELETED),
        DocId(0),
    ]));
    fx.check(Filter::Ids(vec![]));
}

#[test]
fn de_morgan_holds_through_the_evaluator() {
    // An algebraic cross-check: two filters that must denote the same set by
    // construction, evaluated independently. This catches a whole class of
    // composition bug without naming the expected answer at all.
    let fx = fixture();
    let a = Filter::Term(1);
    let b = Filter::Term(2);
    let left = fx.admitted(&Filter::Not(Box::new(Filter::Or(vec![
        a.clone(),
        b.clone(),
    ]))));
    let right = fx.admitted(&Filter::And(vec![
        Filter::Not(Box::new(a.clone())),
        Filter::Not(Box::new(b.clone())),
    ]));
    assert_eq!(left, right, "not(a or b) must equal (not a) and (not b)");

    let left2 = fx.admitted(&Filter::Not(Box::new(Filter::And(vec![
        a.clone(),
        b.clone(),
    ]))));
    let right2 = fx.admitted(&Filter::Or(vec![
        Filter::Not(Box::new(a)),
        Filter::Not(Box::new(b)),
    ]));
    assert_eq!(left2, right2, "not(a and b) must equal (not a) or (not b)");
}

/// Correct answers cannot catch reopening whole-key plans per block. Refuse
/// addressed admission reads and repeated/backward lane positions, then run
/// nested, repeated terms across gaps and worker boundaries on persisted data.
#[test]
fn admission_streams_advance_once_per_term_occurrence() {
    use haiiie_core::{
        Batch, Result, SetSnapshot, SetStore,
        slice::BlockMask,
        store::{Lanes, Version},
    };
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    struct Streaming(YesnoStore, Arc<AtomicUsize>);
    struct Snapshot(<YesnoStore as SetStore>::Snap, Arc<AtomicUsize>);
    struct Cursor {
        inner: Box<dyn Lanes>,
        last: Vec<Option<u64>>,
    }
    impl SetStore for Streaming {
        type Snap = Snapshot;
        fn snapshot(&self) -> Result<Snapshot> {
            Ok(Snapshot(self.0.snapshot()?, self.1.clone()))
        }
        fn write(&self, b: &Batch) -> Result<Version> {
            self.0.write(b)
        }
    }
    impl SetSnapshot for Snapshot {
        fn version(&self) -> Version {
            self.0.version()
        }
        fn load(&self, k: u64) -> Result<Vec<u64>> {
            self.0.load(k)
        }
        fn key_range(&self, lo: u64, hi: u64) -> Result<Vec<u64>> {
            self.0.key_range(lo, hi)
        }
        fn cardinality(&self, k: u64) -> Result<u64> {
            self.0.cardinality(k)
        }
        fn contains(&self, k: u64, o: u64) -> Result<bool> {
            self.0.contains(k, o)
        }
        fn max(&self, k: u64) -> Result<Option<u64>> {
            self.0.max(k)
        }
        fn load_block(&self, k: u64, b: u64, out: &mut BlockMask) -> Result<bool> {
            assert!(
                !matches!((k >> 20) & 255, 0x02 | 0x30),
                "admission reopened a whole-key plan"
            );
            self.0.load_block(k, b, out)
        }
        fn open_lanes(&self, keys: &[u64]) -> Result<Option<Box<dyn Lanes>>> {
            let lanes = self.0.open_lanes(keys)?.unwrap();
            if keys.first().is_some_and(|k| (k >> 20) & 255 == 0x02) {
                self.1.fetch_add(1, Ordering::Relaxed);
                Ok(Some(Box::new(Cursor {
                    inner: lanes,
                    last: vec![None; keys.len()],
                })))
            } else {
                Ok(Some(lanes))
            }
        }
    }
    impl Lanes for Cursor {
        fn read(&mut self, lane: usize, block: u64, out: &mut BlockMask) -> Result<bool> {
            assert!(
                self.last[lane].is_none_or(|last| block > last),
                "term occurrence rewound a lane"
            );
            self.last[lane] = Some(block);
            self.inner.read(lane, block, out)
        }
    }
    let fx = fixture();
    // Extend to six posting blocks so threads(4) actually takes the parallel
    // path, with an empty gap. Same code and terms as a fixture document.
    {
        let mut w = fx.idx.writer();
        w.put(DocId(5 * 65_536), CodeRef::Dense(&[3, 0])).unwrap();
        w.attr(DocId(5 * 65_536), 2).unwrap();
        w.commit().unwrap();
    }
    fx.idx.store().flush().unwrap();
    // Reopen before relying on the persisted stream's behavior.
    let dir = fx._dir.path().join("db");
    drop(fx.idx);
    let raw = Index::open(YesnoStore::open(&dir).unwrap(), 1).unwrap();
    let opens = Arc::new(AtomicUsize::new(0));
    let streaming = Index::open(Streaming(raw.store().clone(), opens.clone()), 1).unwrap();
    let filter = Filter::And(vec![
        Filter::Or(vec![Filter::Term(1), Filter::Term(2), Filter::Term(1)]),
        Filter::Not(Box::new(Filter::And(vec![Filter::Term(2), Filter::None]))),
    ]);
    for path in [
        PathHint::Gather,
        PathHint::DenseScan,
        PathHint::Inverted,
        PathHint::Auto,
    ] {
        for threads in [1, 4] {
            let want = raw
                .search()
                .code(CodeRef::Dense(&[3, 0]))
                .metric(Metric::Hamming)
                .filter(filter.clone())
                .path(path)
                .threads(threads)
                .k(20)
                .execute()
                .unwrap();
            opens.store(0, Ordering::Relaxed);
            let got = streaming
                .search()
                .code(CodeRef::Dense(&[3, 0]))
                .metric(Metric::Hamming)
                .filter(filter.clone())
                .path(path)
                .threads(threads)
                .k(20)
                .execute()
                .unwrap();
            assert_eq!(got.hits, want.hits);
            assert_eq!(
                opens.load(Ordering::Relaxed),
                threads,
                "expected one admission open per worker"
            );
            assert!(got.hits.iter().any(|h| h.id == DocId(5 * 65_536)));
        }
    }
}
