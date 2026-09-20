//! Compaction changes identity, so counts and path agreement alone are weak
//! oracles. Expected codes and attributes are rebuilt from the input fixture,
//! independently sorted by weight and old ID, then compared as complete Hits.
//! Every assertion about committed data uses a reopened directory. The lock
//! and pending marker cover the two boundaries where external IDs could drift.

use std::collections::BTreeSet;
use std::path::Path;

use haiiie_core::{
    Batch, BlockStats, CodeRef, DocId, Error, Filter, Index, Kernel, KeySpace, Metric, PathHint,
    SetSnapshot, SetStore, YesnoStore, acknowledge_compaction, compact,
};
use haiiie_testkit::{Corpus, oracle};
use proptest::prelude::*;

fn directory() -> tempfile::TempDir {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../.agents-workspace/tmp");
    std::fs::create_dir_all(&root).unwrap();
    tempfile::tempdir_in(root).unwrap()
}

const METRICS: [Metric; 4] = [
    Metric::Dot,
    Metric::Hamming,
    Metric::Jaccard,
    Metric::Cosine,
];
const PATHS: [(PathHint, Kernel); 5] = [
    (PathHint::Gather, Kernel::CarrySave),
    (PathHint::DenseScan, Kernel::CarrySave),
    (PathHint::Inverted, Kernel::Tiled),
    (PathHint::Inverted, Kernel::CarrySave),
    (PathHint::Inverted, Kernel::Ripple),
];

fn check_queries(idx: &Index<YesnoStore>, corpus: &Corpus, old_ids: &[DocId]) {
    let admitted: BTreeSet<usize> = old_ids
        .iter()
        .enumerate()
        .filter_map(|(i, old)| old.get().is_multiple_of(2).then_some(i))
        .collect();
    let excluded: BTreeSet<usize> = (0..corpus.len())
        .filter(|i| !admitted.contains(i))
        .collect();
    for metric in METRICS {
        for (path, kernel) in PATHS {
            for (filter, allowed) in [
                (Filter::All, None),
                (Filter::Term(7), Some(&admitted)),
                (Filter::Not(Box::new(Filter::Term(7))), Some(&excluded)),
                (
                    Filter::And(vec![
                        Filter::Term(7),
                        Filter::Term(KeySpace::INDEX_MAX as u32),
                    ]),
                    Some(&admitted),
                ),
            ] {
                for k in [1, 5, corpus.len() + 1] {
                    let q = CodeRef::Dense(&corpus.codes[0]);
                    let got = idx
                        .search()
                        .code(q)
                        .metric(metric)
                        .path(path)
                        .kernel(kernel)
                        .filter(filter.clone())
                        .k(k)
                        .execute()
                        .unwrap();
                    let want = oracle::top_k(corpus, q, metric, k, None, allowed);
                    assert_eq!(got.hits, want, "{metric:?}/{path:?}/{kernel:?}, k={k}");
                }
            }
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(8))]
    #[test]
    fn compaction_preserves_codes_and_filters_across_geometry(
        dims in prop_oneof![Just(1u32), Just(65), Just(129), Just(192), Just(256)],
        input in prop::collection::vec((any::<u64>(), any::<bool>()), 8..24),
    ) {
        let dir = directory();
        let path = dir.path().join("db");
        let mut surviving = Vec::new();
        {
            let idx = Index::create(YesnoStore::open_with_shards(&path, 2).unwrap(), 255, dims).unwrap();
            let mut writer = idx.writer();
            for (i, &(bits, deleted)) in input.iter().enumerate() {
                // Sparse IDs cross both forward-row and posting-block seams.
                let old = DocId(if i < 4 { i as u64 } else { 65_530 + i as u64 });
                let mut code = vec![bits; dims.div_ceil(64) as usize];
                if !dims.is_multiple_of(64) {
                    *code.last_mut().unwrap() &= (1 << (dims % 64)) - 1;
                }
                writer.put(old, CodeRef::Dense(&code)).unwrap();
                if old.get().is_multiple_of(2) { writer.attr(old, 7).unwrap(); }
                writer.attr(old, KeySpace::INDEX_MAX as u32).unwrap();
                // Keep at least one document for non-vacuous score checks.
                if deleted && i != 0 { writer.delete(old).expect("delete"); }
                else { surviving.push((old, code)); }
            }
            writer.commit().unwrap();
            idx.refresh_stats().unwrap();
            idx.store().flush().unwrap();
        }
        surviving.sort_by_key(|(id, code)| (std::cmp::Reverse(code.iter().map(|b| b.count_ones()).sum::<u32>()), *id));
        let want_ids: Vec<DocId> = surviving.iter().map(|(old, _)| *old).collect();
        let want_codes = Corpus { dims, codes: surviving.into_iter().map(|(_, code)| code).collect() };
        let remap = compact(&path, 255).unwrap();
        prop_assert_eq!(&remap.old_ids, &want_ids);
        // compact() reopens the directory each time: the returned mapping is
        // reconstructed from durable storage, not the first call's memory.
        prop_assert_eq!(compact(&path, 255).unwrap(), remap.clone());
        prop_assert!(matches!(Index::open(YesnoStore::open(&path).unwrap(), 255), Err(Error::CompactionPending(255))));
        acknowledge_compaction(&path, 255, remap.source_version).unwrap();
        let idx = Index::open(YesnoStore::open(&path).unwrap(), 255).unwrap();
        check_queries(&idx, &want_codes, &want_ids);
        let snap = idx.store().snapshot().unwrap();
        prop_assert!(snap.load(idx.keys().stat(1)).unwrap().is_empty());
        prop_assert!(snap.load(idx.keys().remap()).unwrap().is_empty());
        prop_assert!(idx.store().db().verify().unwrap().iter().all(|r| r.is_clean()));
    }
}

#[test]
fn retired_ids_are_clean_and_other_namespaces_are_untouched() {
    let dir = directory();
    let path = dir.path().join("db");
    {
        let store = YesnoStore::open_with_shards(&path, 2).unwrap();
        for namespace in [1, 2] {
            let idx = Index::create(store.clone(), namespace, 64).unwrap();
            let mut w = idx.writer();
            w.put(DocId(0), CodeRef::Dense(&[u64::MAX])).unwrap();
            w.put(DocId(4), CodeRef::Dense(&[1])).unwrap();
            w.attr(DocId(0), 19).unwrap();
            w.attr(DocId(4), 23).unwrap();
            if namespace == 1 {
                w.delete(DocId(4)).expect("delete");
            }
            w.commit().unwrap();
            idx.refresh_stats().unwrap();
        }
        store.flush().unwrap();
    }
    let remap = compact(&path, 1).unwrap();
    assert_eq!(remap.old_ids, [DocId(0)]);
    {
        let store = YesnoStore::open(&path).unwrap();
        let other = Index::open(store, 2).unwrap();
        let got = other
            .search()
            .code(CodeRef::Dense(&[1]))
            .k(10)
            .execute()
            .unwrap();
        assert_eq!(
            got.hits
                .iter()
                .map(|h| (h.id, h.weight))
                .collect::<Vec<_>>(),
            [(DocId(4), 1), (DocId(0), 64)]
        );
    }
    acknowledge_compaction(&path, 1, remap.source_version).unwrap();
    {
        let idx = Index::open(YesnoStore::open(&path).unwrap(), 1).unwrap();
        let mut w = idx.writer();
        w.put(DocId(4), CodeRef::Dense(&[2])).unwrap();
        w.commit().unwrap();
        idx.store().flush().unwrap();
    }
    let idx = Index::open(YesnoStore::open(&path).unwrap(), 1).unwrap();
    for (path, kernel) in PATHS {
        let got = idx
            .search()
            .code(CodeRef::Dense(&[2]))
            .path(path)
            .kernel(kernel)
            .filter(Filter::Ids(vec![DocId(4)]))
            .k(1)
            .execute()
            .unwrap();
        assert_eq!((got.hits[0].inter, got.hits[0].weight), (1, 1));
        assert_eq!(idx.search().filter(Filter::Term(23)).count().unwrap(), 0);
    }
}

#[test]
fn empty_and_fully_deleted_indexes_have_recoverable_empty_mappings() {
    for deleted in [false, true] {
        let dir = directory();
        let path = dir.path().join("db");
        {
            let idx = Index::create(YesnoStore::open_with_shards(&path, 1).unwrap(), 0, 1).unwrap();
            if deleted {
                let mut w = idx.writer();
                w.put(DocId(65_536), CodeRef::Dense(&[1])).unwrap();
                w.attr(DocId(65_536), 7).unwrap();
                w.commit().unwrap();
                idx.refresh_stats().unwrap();
                let mut w = idx.writer();
                w.delete(DocId(65_536)).expect("delete");
                w.commit().unwrap();
            }
        }
        let remap = compact(&path, 0).unwrap();
        assert!(remap.old_ids.is_empty());
        assert_eq!(compact(&path, 0).unwrap(), remap);
        acknowledge_compaction(&path, 0, remap.source_version).unwrap();
        let idx = Index::open(YesnoStore::open(&path).unwrap(), 0).unwrap();
        assert!(idx.is_empty().unwrap());
        assert_eq!(
            idx.store().db().snapshot().unwrap().keys().unwrap(),
            [idx.keys().meta()]
        );
    }
}

#[test]
fn compaction_requires_exclusive_access_and_the_matching_acknowledgement() {
    let dir = directory();
    let path = dir.path().join("db");
    let idx = Index::create(YesnoStore::open_with_shards(&path, 1).unwrap(), 0, 64).unwrap();
    let mut writer = idx.writer();
    writer.put(DocId(3), CodeRef::Dense(&[7])).unwrap();
    // A pending writer keeps the original database open: compaction cannot
    // overtake it and later accept a write using an old ID.
    assert!(compact(&path, 0).is_err());
    writer.commit().unwrap();
    drop(idx);
    let remap = compact(&path, 0).unwrap();
    assert!(matches!(
        acknowledge_compaction(&path, 0, remap.source_version + 1),
        Err(Error::CompactionMismatch(_))
    ));
    assert_eq!(compact(&path, 0).unwrap(), remap);
    acknowledge_compaction(&path, 0, remap.source_version).unwrap();
    let next = compact(&path, 0).unwrap();
    assert_ne!(next.source_version, remap.source_version);
    assert!(matches!(
        acknowledge_compaction(&path, 0, remap.source_version),
        Err(Error::CompactionMismatch(_))
    ));
    assert_eq!(compact(&path, 0).unwrap(), next);
    acknowledge_compaction(&path, 0, next.source_version).unwrap();
}

#[test]
fn compaction_crosses_mapping_and_posting_blocks_and_tightens_statistics() {
    let dir = directory();
    let path = dir.path().join("db");
    // Exactly two posting blocks, each with alternating zero/full codes.
    // Sorting must turn them into one uniform block of each weight; this is a
    // semantic threshold derived from the construction, not a timing budget.
    let n = 131_072u64;
    {
        let idx = Index::create(YesnoStore::open_with_shards(&path, 2).unwrap(), 0, 1).unwrap();
        let mut w = idx.writer();
        for id in 0..n {
            w.put(DocId(id), CodeRef::Dense(&[id % 2])).unwrap();
            w.flush_if_large(100_000).unwrap();
        }
        w.commit().unwrap();
        idx.refresh_stats().unwrap();
        assert_eq!(
            idx.search()
                .metric(Metric::Jaccard)
                .explain()
                .unwrap()
                .block_weight_spread,
            Some(1)
        );
        idx.store().flush().unwrap();
    }
    let remap = compact(&path, 0).unwrap();
    let expected: Vec<_> = (1..n)
        .step_by(2)
        .chain((0..n).step_by(2))
        .map(DocId)
        .collect();
    assert_eq!(remap.old_ids, expected);
    assert_eq!(compact(&path, 0).unwrap(), remap);
    acknowledge_compaction(&path, 0, remap.source_version).unwrap();
    let idx = Index::open(YesnoStore::open(&path).unwrap(), 0).unwrap();
    assert_eq!(
        idx.search()
            .metric(Metric::Jaccard)
            .explain()
            .unwrap()
            .block_weight_spread,
        Some(0)
    );
    let snap = idx.store().snapshot().unwrap();
    for (block, weight) in [(0, 1), (1, 0)] {
        assert_eq!(
            BlockStats::from_ordinals(&snap.load(idx.keys().stat(block)).unwrap()),
            Some(BlockStats {
                w_min: weight,
                w_max: weight
            })
        );
    }
    for metric in METRICS {
        for (path, kernel) in PATHS {
            let got = idx
                .search()
                .code(CodeRef::Dense(&[1]))
                .metric(metric)
                .path(path)
                .kernel(kernel)
                .k(3)
                .execute()
                .unwrap();
            assert_eq!(
                got.hits
                    .iter()
                    .map(|h| (h.id, h.inter, h.weight))
                    .collect::<Vec<_>>(),
                [(DocId(0), 1, 1), (DocId(1), 1, 1), (DocId(2), 1, 1)]
            );
        }
    }
    assert!(
        idx.store()
            .db()
            .verify()
            .unwrap()
            .iter()
            .all(|r| r.is_clean())
    );
}

#[test]
fn malformed_recovery_mapping_is_refused_without_acknowledging() {
    let dir = directory();
    let path = dir.path().join("db");
    {
        let idx = Index::create(YesnoStore::open_with_shards(&path, 1).unwrap(), 0, 1).unwrap();
        let mut w = idx.writer();
        w.put(DocId(0), CodeRef::Dense(&[0])).unwrap();
        w.put(DocId(1), CodeRef::Dense(&[1])).unwrap();
        w.commit().unwrap();
    }
    let remap = compact(&path, 0).unwrap();
    {
        let store = YesnoStore::open(&path).unwrap();
        let mut b = Batch::new();
        // Turn the second old ID into one while retaining row presence. The
        // cardinalities still agree; only a bijection check catches the fault.
        b.insert(KeySpace::new(0).remap(), 64);
        store.write(&b).unwrap();
    }
    assert!(matches!(compact(&path, 0), Err(Error::CorruptMeta(_))));
    assert!(matches!(
        acknowledge_compaction(&path, 0, remap.source_version),
        Err(Error::CorruptMeta(_))
    ));
    assert!(matches!(
        Index::open(YesnoStore::open(&path).unwrap(), 0),
        Err(Error::CompactionPending(0))
    ));
}

#[test]
fn invalid_source_rows_are_refused_before_any_rewrite() {
    let dir = directory();
    let path = dir.path().join("db");
    let (version, original) = {
        let idx = Index::create(YesnoStore::open_with_shards(&path, 2).unwrap(), 0, 65).unwrap();
        let mut w = idx.writer();
        w.put(DocId(0), CodeRef::Dense(&[7, 0])).unwrap();
        w.commit().unwrap();
        let mut b = Batch::new();
        b.insert(idx.keys().forward(), 100); // Padding, outside the 65-bit code.
        idx.store().write(&b).unwrap();
        idx.store().flush().unwrap();
        let snap = idx.store().db().snapshot().unwrap();
        let original: Vec<_> = snap
            .keys()
            .unwrap()
            .into_iter()
            .map(|key| (key, snap.load(key).unwrap().iter().collect::<Vec<_>>()))
            .collect();
        (snap.version(), original)
    };
    assert!(matches!(compact(&path, 0), Err(Error::CorruptMeta(_))));
    let store = YesnoStore::open(&path).unwrap();
    let snap = store.db().snapshot().unwrap();
    assert_eq!(snap.version(), version);
    let after: Vec<_> = snap
        .keys()
        .unwrap()
        .into_iter()
        .map(|key| (key, snap.load(key).unwrap().iter().collect::<Vec<_>>()))
        .collect();
    assert_eq!(
        after, original,
        "refusal changed source data or wrote a pending marker"
    );
}
