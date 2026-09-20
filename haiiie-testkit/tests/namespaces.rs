//! Two indexes in one database, and wrapping a database opened elsewhere.
//!
//! # Why this file exists
//!
//! `YesnoStore::from_db` had **no caller anywhere in the workspace** -- found by
//! auditing public items. It is not dead: it is the escape hatch for a caller
//! who opens the database itself, with its own options, and hands it over. The
//! right response to a public item with no caller is not always deletion, and
//! here it is a test.
//!
//! The namespace property is the other half. A key is
//! `(namespace << 56) | (kind << 20) | index`, and the layout module says the
//! namespace exists so that several haiiie indexes, or one haiiie index and an
//! application's own keys, can share a database without collision. That is a
//! design claim about the key space and nothing exercised it: every test in the
//! suite uses one namespace, and most use namespace 1.
//!
//! A failure would be silent in the worst way -- two indexes reading each
//! other's posting lists return wrong documents with no error, and the
//! single-namespace suite cannot see it.

use haiiie_core::{CodeRef, DocId, Index, Metric, PathHint, SetStore, YesnoStore};

const DIMS: u32 = 64;

fn code(bits: &[u32]) -> Vec<u64> {
    let mut v = vec![0u64; 1];
    for &b in bits {
        v[0] |= 1u64 << (b & 63);
    }
    v
}

fn ids_for(idx: &Index<YesnoStore>, q: &[u64]) -> Vec<u64> {
    let mut v: Vec<u64> = idx
        .search()
        .code(CodeRef::Dense(q))
        .metric(Metric::Hamming)
        .k(64)
        .path(PathHint::Inverted)
        .execute()
        .expect("search")
        .hits
        .iter()
        .map(|h| h.id.get())
        .collect();
    v.sort_unstable();
    v
}

#[test]
fn two_namespaces_in_one_database_do_not_see_each_other() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("db");

    // Namespace 7 holds ids 0..4. Dropped before the next open, because the
    // database takes an exclusive directory lock.
    {
        let idx = Index::create(YesnoStore::open(&path).expect("open"), 7, DIMS).expect("create");
        let mut w = idx.writer();
        for i in 0..4u64 {
            w.put(DocId(i), CodeRef::Dense(&code(&[1, 2, 3])))
                .expect("put");
            w.attr(DocId(i), 5).expect("attr");
        }
        w.commit().expect("commit");
    }
    // Namespace 8 holds ids 100..104, in the same file, with the same attribute
    // term and overlapping codes -- everything that would collide if the
    // namespace were not in the key.
    {
        let idx = Index::create(YesnoStore::open(&path).expect("open"), 8, DIMS).expect("create");
        let mut w = idx.writer();
        for i in 100..104u64 {
            w.put(DocId(i), CodeRef::Dense(&code(&[1, 2, 3])))
                .expect("put");
            w.attr(DocId(i), 5).expect("attr");
        }
        w.commit().expect("commit");
    }

    let q = code(&[1, 2, 3]);
    {
        let idx = Index::open(YesnoStore::open(&path).expect("open"), 7).expect("open 7");
        idx.refresh_stats().expect("stats");
        assert_eq!(
            idx.len().expect("len"),
            4,
            "namespace 7 counted its neighbour"
        );
        assert_eq!(ids_for(&idx, &q), vec![0, 1, 2, 3]);
        assert_eq!(
            idx.search()
                .filter(haiiie_core::Filter::Term(5))
                .count()
                .expect("count"),
            4,
            "an attribute term leaked across namespaces"
        );
    }
    {
        let idx = Index::open(YesnoStore::open(&path).expect("open"), 8).expect("open 8");
        idx.refresh_stats().expect("stats");
        assert_eq!(idx.len().expect("len"), 4);
        assert_eq!(ids_for(&idx, &q), vec![100, 101, 102, 103]);
    }
}

/// A namespace's metadata, geometry and statistics are its own.
///
/// Two indexes of **different widths** in one database is the case where a
/// leaked metadata blob is unmistakable: the reader would decode the wrong
/// geometry and mis-address every forward row rather than merely returning
/// extra documents.
#[test]
fn namespaces_may_differ_in_geometry() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("db");
    {
        Index::create(YesnoStore::open(&path).expect("open"), 1, 64).expect("create narrow");
    }
    {
        Index::create(YesnoStore::open(&path).expect("open"), 2, 256).expect("create wide");
    }
    let narrow = Index::open(YesnoStore::open(&path).expect("open"), 1).expect("open 1");
    assert_eq!(narrow.meta().dims, 64);
    drop(narrow);
    let wide = Index::open(YesnoStore::open(&path).expect("open"), 2).expect("open 2");
    assert_eq!(wide.meta().dims, 256);
}

/// `from_db` wraps a database the caller opened, with the caller's own options.
///
/// The point of the method is that those options are not haiiie's to choose --
/// anything `DbOptions` exposes and `YesnoStore::open` does not is reachable
/// only this way.
#[test]
fn a_database_opened_by_the_caller_can_be_wrapped() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("db");
    let db = yesno_core::Db::open(&path).expect("open db");
    let idx = Index::create(YesnoStore::from_db(db), 3, DIMS).expect("create");

    let mut w = idx.writer();
    w.put(DocId(0), CodeRef::Dense(&code(&[7, 8])))
        .expect("put");
    w.commit().expect("commit");
    idx.refresh_stats().expect("stats");
    assert_eq!(ids_for(&idx, &code(&[7, 8])), vec![0]);

    // And it is the same database on disk: reopened the ordinary way, the
    // document is there. A wrapper that quietly used somewhere else would pass
    // every assertion above.
    idx.store().flush().expect("checkpoint");
    drop(idx);
    let reopened = Index::open(YesnoStore::open(&path).expect("reopen"), 3).expect("open");
    assert_eq!(reopened.len().expect("len"), 1);
}
