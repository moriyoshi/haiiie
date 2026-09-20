//! A `put` of an id already written in the same batch.
//!
//! # Why this file exists
//!
//! The writer carries two per-block caches -- `marked`, recording what the batch
//! has written, and the live-block mask behind `is_present`. Both held **one**
//! block and were replaced when a `put` reached another. Ascending ingest never
//! revisits a block, so neither was ever wrong on any path this suite drove, and
//! both were wrong for an update batch keyed by arbitrary ids, which the API
//! permits and the gRPC ingest path passes straight through.
//!
//! The symptom is a silently wrong row rather than an error:
//! `put(0, a); put(70_000, b); put(0, c)` left doc 0 holding `a | c`, weight 6
//! where the last code has 3. Every search then answers from it, correctly, and
//! reports a document that was never stored.
//!
//! # How it was found, because the method transfers
//!
//! Not by a failing test -- by asking, on upstream's prompting, which mechanisms
//! have only ever been observed in their quiet state. The blind-clearing
//! fallback had never run in any test: it is reached only by a within-batch
//! re-put, and nothing here did one. **A branch whose only observed state is the
//! one where it does nothing is untested by construction**, and the passing case
//! looks identical whether it works or not.
//!
//! The two targeted tests below pin the two instances. The third is the class:
//! random ids, random interleaving, duplicates on purpose, against last-write-
//! wins. It would have caught both without anyone knowing the caches existed.

use haiiie_core::{CodeRef, DocId, Index, Metric, PathHint, YesnoStore};

fn code(bits: &[u32], words: usize) -> Vec<u64> {
    let mut v = vec![0u64; words];
    for &b in bits {
        v[(b as usize) >> 6] |= 1u64 << (b & 63);
    }
    v
}

#[test]
fn a_re_put_after_touching_another_block_still_lands_the_last_code() {
    let dir = tempfile::tempdir().expect("tempdir");
    let dims = 256u32;
    let words = 4usize;
    let idx = Index::create(
        YesnoStore::open(dir.path().join("db")).expect("open"),
        1,
        dims,
    )
    .expect("create");

    let o = code(&[1, 2, 3], words);
    let a = code(&[10, 11, 12], words);
    let c = code(&[20, 21, 22], words);
    let b_other = code(&[30], words);

    // Commit the original row, so `present` is true on the next batch.
    {
        let mut w = idx.writer();
        w.put(DocId(0), CodeRef::Dense(&o)).expect("put");
        w.commit().expect("commit");
    }

    // One batch: put id 0, then an id in a DIFFERENT block, then id 0 again.
    {
        let mut w = idx.writer();
        w.put(DocId(0), CodeRef::Dense(&a)).expect("put");
        w.put(DocId(70_000), CodeRef::Dense(&b_other)).expect("put");
        w.put(DocId(0), CodeRef::Dense(&c)).expect("put");
        w.commit().expect("commit");
    }
    idx.refresh_stats().expect("stats");

    // Query with `c` itself. If the row is right, doc 0 has intersection 3 and
    // weight 3. If the batch left `a`'s bits behind, the weight is larger.
    let got = idx
        .search()
        .code(CodeRef::Dense(&c))
        .metric(Metric::Hamming)
        .k(5)
        .path(PathHint::Gather)
        .execute()
        .expect("search");
    let hit = got.hits.iter().find(|h| h.id == DocId(0)).expect("doc 0");
    assert_eq!(
        (hit.inter, hit.weight),
        (3, 3),
        "doc 0's row is not exactly the last code put in the batch: {hit:?}"
    );
}

/// The same interleaving, but for an id the store has **never** held. The guard
/// that matters here is `is_present`, whose live-block cache is also per-block:
/// a fresh id marked in block 0 is forgotten when the cache moves to block 1, so
/// the re-put sees "not present", clears nothing, and ORs the two codes.
#[test]
fn a_re_put_of_a_brand_new_id_across_blocks_also_lands_the_last_code() {
    let dir = tempfile::tempdir().expect("tempdir");
    let dims = 256u32;
    let words = 4usize;
    let idx = Index::create(
        YesnoStore::open(dir.path().join("db")).expect("open"),
        1,
        dims,
    )
    .expect("create");

    let a = code(&[10, 11, 12], words);
    let c = code(&[20, 21, 22], words);
    let b_other = code(&[30], words);

    // No prior commit: id 0 is new to the store in this very batch.
    {
        let mut w = idx.writer();
        w.put(DocId(0), CodeRef::Dense(&a)).expect("put");
        w.put(DocId(70_000), CodeRef::Dense(&b_other)).expect("put");
        w.put(DocId(0), CodeRef::Dense(&c)).expect("put");
        w.commit().expect("commit");
    }
    idx.refresh_stats().expect("stats");

    let got = idx
        .search()
        .code(CodeRef::Dense(&c))
        .metric(Metric::Hamming)
        .k(5)
        .path(PathHint::Gather)
        .execute()
        .expect("search");
    let hit = got.hits.iter().find(|h| h.id == DocId(0)).expect("doc 0");
    assert_eq!(
        (hit.inter, hit.weight),
        (3, 3),
        "a brand-new id re-put across blocks kept both codes: {hit:?}"
    );
}

/// The class, rather than the two instances: random ids over several blocks,
/// deliberately repeated, checked against last-write-wins.
///
/// `put` is specified as a replacement, so the stored row must equal the *last*
/// code written for that id and nothing else. The oracle is a map, which is the
/// slow obvious implementation of exactly that sentence.
#[test]
fn the_last_put_of_an_id_in_a_batch_is_the_one_that_survives() {
    use std::collections::HashMap;

    let dir = tempfile::tempdir().expect("tempdir");
    let dims = 256u32;
    let words = 4usize;
    let idx = Index::create(
        YesnoStore::open(dir.path().join("db")).expect("open"),
        1,
        dims,
    )
    .expect("create");

    // Ids drawn from three blocks so the caches are forced to move, with
    // repeats guaranteed by drawing from a small pool per block.
    let ids: Vec<u64> = (0..3)
        .flat_map(|b: u64| (0..6).map(move |i| b * 70_000 + i))
        .collect();
    let mut want: HashMap<u64, Vec<u64>> = HashMap::new();
    let mut s = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        s
    };

    {
        let mut w = idx.writer();
        for round in 0..4 {
            for &id in &ids {
                // A distinct code per (round, id), three bits wide.
                let base = ((next() % 80) * 3) as u32;
                let c = code(&[base, base + 1, base + 2], words);
                w.put(DocId(id), CodeRef::Dense(&c)).expect("put");
                want.insert(id, c);
            }
            // Interleave the blocks differently each round.
            if round % 2 == 0 {
                for &id in ids.iter().rev() {
                    let base = ((next() % 80) * 3) as u32;
                    let c = code(&[base, base + 1, base + 2], words);
                    w.put(DocId(id), CodeRef::Dense(&c)).expect("put");
                    want.insert(id, c);
                }
            }
        }
        w.commit().expect("commit");
    }
    idx.refresh_stats().expect("stats");

    // Every document must score exactly against its own final code: a row that
    // kept an earlier code has a larger weight and the mismatch is visible.
    for (&id, c) in &want {
        let got = idx
            .search()
            .code(CodeRef::Dense(c))
            .metric(Metric::Hamming)
            .k(ids.len())
            .path(PathHint::Gather)
            .execute()
            .expect("search");
        let hit = got
            .hits
            .iter()
            .find(|h| h.id == DocId(id))
            .unwrap_or_else(|| panic!("doc {id} missing from its own query"));
        assert_eq!(
            (hit.inter, hit.weight),
            (3, 3),
            "doc {id} did not end at its last code"
        );
    }
}
