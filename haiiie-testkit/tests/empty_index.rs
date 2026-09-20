//! What a search reports when there is nothing to search.
//!
//! An index with no live documents is not an exotic state: it is every index
//! between `create` and the first commit, and every index whose documents have
//! all been deleted. The answer has always been right -- no hits -- but the
//! **statistics** were not, because `0 ..= 0` is one iteration and the scan
//! could not tell "the last block is block zero" from "there is no last block".
//!
//! Worth a test rather than a fix alone, because nothing else in the suite
//! looks at `ScanStats` on a corpus this small, and a count that is wrong by one
//! is exactly the kind of thing a differential oracle cannot see: the oracle
//! checks hits, and the hits were never wrong.

use haiiie_core::{DocId, Filter, Index, Metric, PathHint, YesnoStore};
use haiiie_testkit::{Corpus, Shape};

fn empty() -> Index<YesnoStore> {
    let dir = tempfile::tempdir().expect("tempdir");
    // The directory outlives the index only because the store holds it open;
    // leaking it is deliberate and confined to this test.
    let path = dir.keep();
    Index::create(YesnoStore::open(path.join("db")).expect("open"), 1, 64).expect("create")
}

#[test]
fn a_search_over_no_documents_visits_no_blocks() {
    let idx = empty();
    let q = [0b1011u64];

    for threads in [1usize, 4] {
        for path in [
            PathHint::Auto,
            PathHint::Inverted,
            PathHint::Gather,
            PathHint::DenseScan,
        ] {
            let hits = idx
                .search()
                .code(haiiie_core::CodeRef::Dense(&q))
                .metric(Metric::Hamming)
                .k(10)
                .threads(threads)
                .path(path)
                .execute()
                .expect("search");
            assert!(hits.hits.is_empty(), "{path:?} at {threads} threads");
            assert_eq!(hits.scored, 0, "{path:?} at {threads} threads");
            assert_eq!(
                hits.stats.blocks_visited, 0,
                "{path:?} at {threads} threads: visited a block"
            );
            assert_eq!(
                hits.stats.blocks_skipped, 0,
                "{path:?} at {threads} threads: skipped a block that does not exist"
            );
        }
    }
    assert_eq!(idx.search().filter(Filter::All).count().expect("count"), 0);
    assert_eq!(idx.search().explain().expect("explain").blocks, 0);
}

/// The same, after every document has been deleted.
///
/// Distinct from the case above: this index *has* had blocks, so the `LIVE` key
/// exists and has been written to. A fix that special-cased a never-written key
/// rather than an empty one would pass the first test and fail this.
#[test]
fn a_search_after_deleting_everything_visits_no_blocks() {
    let idx = empty();
    let c = Corpus::generate(9, 64, 200, Shape::Balanced);
    {
        let mut w = idx.writer();
        for i in 0..c.len() {
            w.put(DocId(i as u64), c.code(i)).expect("put");
        }
        w.commit().expect("commit");
    }
    let before = idx
        .search()
        .code(c.code(0))
        .metric(Metric::Hamming)
        .k(10)
        .execute()
        .expect("search");
    assert_eq!(before.stats.blocks_visited, 1, "setup: one block held them");

    {
        let mut w = idx.writer();
        for i in 0..c.len() {
            w.delete(DocId(i as u64)).expect("delete");
        }
        w.commit().expect("commit");
    }
    let after = idx
        .search()
        .code(c.code(0))
        .metric(Metric::Hamming)
        .k(10)
        .execute()
        .expect("search");
    assert!(after.hits.is_empty());
    assert_eq!(after.stats.blocks_visited, 0);
    assert_eq!(
        after.stats.blocks_skipped, 0,
        "an emptied index still reports a block"
    );
}

/// A foreign key in any reserved kind must be refused before META makes the
/// application ordinals look like index data. Gaps and other namespaces remain
/// available to applications sharing the store.
#[test]
fn creation_refuses_occupied_kind_ranges() {
    use haiiie_core::store::{Batch, SetStore};
    use haiiie_core::{Error, KeySpace, Kind};
    use haiiie_testkit::MemStore;

    let keys = KeySpace::new(0);
    for (kind, index) in [
        (Kind::Live, 0),
        (Kind::Dim, 9),
        (Kind::ZPlane, 3),
        (Kind::Forward, 0),
        (Kind::Stat, 17),
        (Kind::Attr, KeySpace::INDEX_MAX),
        (Kind::Compaction, 0),
        (Kind::Remap, 0),
    ] {
        let store = MemStore::new();
        let key = ((kind as u64) << 20) | index;
        let mut batch = Batch::new();
        batch.insert(key, 500);
        store.write(&batch).expect("foreign write");
        assert!(matches!(
            Index::create(store, 0, 64),
            Err(Error::KeyspaceOccupied { namespace: 0, key: found }) if found == key
        ));
    }

    let store = MemStore::new();
    let mut batch = Batch::new();
    batch.insert(3 << 20, 500); // Unassigned kind in namespace zero.
    batch.insert((1 << 56) | keys.live(), 501); // Another namespace.
    store.write(&batch).expect("foreign writes");
    Index::create(store, 0, 64).expect("unoccupied defined kinds");
}

#[test]
fn real_store_refuses_foreign_live_key_in_namespace_zero() {
    use haiiie_core::store::{Batch, SetStore};
    use haiiie_core::{Error, KeySpace};

    let dir = tempfile::tempdir().expect("tempdir");
    let store = YesnoStore::open(dir.path().join("db")).expect("open");
    let mut batch = Batch::new();
    batch.insert(KeySpace::new(0).live(), 500);
    store.write(&batch).expect("foreign write");
    assert!(matches!(
        Index::create(store, 0, 64),
        Err(Error::KeyspaceOccupied {
            namespace: 0,
            key: 2097152
        })
    ));
}
