//! Overwrite-heavy workloads, and the store's own opinion of itself afterwards.
//!
//! # Why this shape, and why it was missing
//!
//! Every other suite here **grows an index monotonically**: documents go in,
//! nothing is rewritten, and the storage engine's allocator never has to free a
//! region and hand it back out. A real ingest does not look like that. A bulk
//! load with `flush_if_large` rewrites each dimension's posting list on every
//! commit, so as a key's chunks grow denser they cross size-class boundaries and
//! their old extents are freed and reallocated -- and updates do the same thing
//! for the life of the index.
//!
//! That difference is not hypothetical. A storage-engine defect found from this
//! project on 2026-09-15 needed exactly it: a slab emptied by reclamation stayed
//! registered as one size class's allocation target, was re-initialized for a
//! *different* class, and then served both at once, so two slot sizes indexed
//! one occupancy bitmap and chunks overlapped. It could not occur without a
//! free-then-reallocate, so no monotonic test could have produced it, and
//! neither could any of ours.
//!
//! # These tests assert on `verify()`, not only on answers
//!
//! That defect was **loud** on a 1024-key index and could have been silent on a
//! smaller one: the engine's trailer and checksum checks are skipped when a cell
//! is not slot-aligned or its slab is packed. A search returning the right
//! documents is therefore not evidence that the store is intact, which makes
//! this the one place in the suite where the oracle is not enough.
//!
//! # The sizes are calibrated against the real defect, and volume is what matters
//!
//! Upstream supplied a patch reversing their fix. It was applied to a **vendored
//! copy** of the engine under `.agents-workspace/tmp` -- never the shared
//! checkout, which other sessions build from -- so this test could be run
//! against the bug it was written for. Measured, first case only:
//!
//! ```text
//! documents  dims  rounds  shards   against the reverted fix
//!      6 000   128       5      32   passes -- blind
//!     15 000   256       4      32   passes -- blind
//!     15 000   256       4       4   passes -- blind
//!     25 000   256       4      32   passes -- blind
//!     40 000   256       4       4   passes -- blind
//!     40 000   256       4      32   FAILS at round 3, density 160
//! ```
//!
//! So **document count is the variable**. An earlier version of this comment
//! asserted the opposite -- that the trigger was the density sweep, that volume
//! did not matter, and that anyone needing more power should add rounds first.
//! It was written before any of these runs existed and is exactly backwards.
//! Shard count matters too, and not in the intuitive direction: four shards is
//! blind where thirty-two catches, so *concentrating* the allocator's work makes
//! this weaker.
//!
//! **It cost about 320 seconds when that table was measured and costs about 34
//! now** ( 32.8 for the first test, 3.2 for the reopen, re-measured 2026-09-18 ).
//! Nothing here changed: the writer stopped clearing a row it was about to
//! rewrite, and a whole-row clear became one range operation instead of
//! `row_bits` of them. The old figure survived both.
//!
//! That is worth correcting rather than leaving, because of what the sentence
//! was *for*. It argued against trimming this file -- several times the rest of
//! the gate, paid for the only configuration shown to catch a silent
//! data-corruption bug. A reader deciding whether to cut the document count
//! would have been weighing a cost ten times the real one, against a table that
//! says 40 000 documents is the line between catching that bug and not. **The
//! table is unaffected**: it records what catches, not what it costs, and it
//! still should not be reduced without re-running it.
//!
//! # What is calibrated and what is not
//!
//! The **first** test is calibrated as above: it fails against the reverted fix
//! and passes with it.
//!
//! The **reopen** test is not, and could not be with that patch. The reversal
//! removes the retirement call from the engine's free path only, leaving the one
//! on its open path intact, so the reopen case never ran against a broken build.
//! It is kept because the open path restores allocator occupancy from stored
//! metadata and is a distinct way for the same class of defect to appear -- but
//! it is an untested test, and saying so is the whole of its warranty.

use haiiie_core::{DocId, Index, Metric, PathHint, SetStore, YesnoStore};
use haiiie_testkit::{Corpus, oracle};
use yesno_core::store::fsck::FsckReport;

/// Sweep a document's code across size classes by varying how many bits it sets.
///
/// A posting list's container -- and therefore the extent size class its chunk
/// lands in -- follows its density, so driving density up and down is what makes
/// the allocator free a region in one class and reallocate it in another.
fn code_at_density(dims: u32, seed: u64, fill: u32, out: &mut [u64]) {
    out.fill(0);
    let mut s = seed | 1;
    for _ in 0..fill {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        let b = (s % u64::from(dims)) as usize;
        out[b >> 6] |= 1u64 << (b & 63);
    }
}

/// Assert the store is intact, letting the storage engine's own predicate decide.
///
/// # The predicate decides, the fields only explain
///
/// This used to test four fields by hand and had **missed a fifth** -- `leaked`,
/// the slots the allocator marks used that nothing references, which is the
/// precise residue a bad free-then-reallocate leaves and therefore the one
/// finding this file exists to provoke. `FsckReport::is_clean` covers all five
/// and existed the whole time; the hand-rolled version was a copy of a
/// definition, which is a thing that can fall behind its original.
///
/// So `is_clean` is the gate and the per-field walk below is only there to say
/// *which* category failed. That split matters more than it looks: if upstream
/// adds a sixth finding, the gate still fires, and the breakdown reports that it
/// could not attribute the failure rather than quietly passing. **A summary that
/// cannot explain a failure is better than a summary that cannot see one.**
/// Why each shard's report is not clean, as lines a reader can act on.
///
/// Separated from the assertion so the **unattributable** case can be tested.
/// That branch runs only when the report grows a finding this breakdown does not
/// know about, which is to say never until upstream adds one -- and a branch
/// whose only observed state is the silent one is untested by construction.
fn trouble_in(reports: &[FsckReport]) -> Vec<String> {
    let mut trouble = Vec::new();
    for (i, r) in reports.iter().enumerate() {
        if r.is_clean() {
            continue;
        }
        let before = trouble.len();
        if !r.errors.is_empty() {
            trouble.push(format!(
                "shard {i}: {:?}",
                &r.errors[..r.errors.len().min(3)]
            ));
        }
        if !r.dangling.is_empty() {
            trouble.push(format!("shard {i}: {} dangling chunk(s)", r.dangling.len()));
        }
        if !r.dangling_nodes.is_empty() {
            trouble.push(format!(
                "shard {i}: {} dangling node(s)",
                r.dangling_nodes.len()
            ));
        }
        if !r.packed_live_mismatch.is_empty() {
            trouble.push(format!(
                "shard {i}: {} packed mismatch",
                r.packed_live_mismatch.len()
            ));
        }
        // Waste rather than corruption, and recoverable -- but not clean, and
        // this is the one the hand-rolled predicate omitted.
        if !r.leaked.is_empty() {
            let slots: u32 = r.leaked.iter().map(|&(_, n)| n).sum();
            trouble.push(format!(
                "shard {i}: {slots} leaked slot(s) across {} size class(es)",
                r.leaked.len()
            ));
        }
        if trouble.len() == before {
            trouble.push(format!(
                "shard {i}: is_clean() is false and no category here matched -- the \
                 report has grown a finding this breakdown does not know about"
            ));
        }
    }
    trouble
}

/// Assert the store is intact, letting the storage engine's own predicate decide.
///
/// # The predicate decides, the fields only explain
///
/// This used to test four fields by hand and had **missed a fifth** -- `leaked`,
/// the slots the allocator marks used that nothing references, which is the
/// precise residue a bad free-then-reallocate leaves and therefore the one
/// finding this file exists to provoke. `FsckReport::is_clean` covers all five
/// and existed the whole time; the hand-rolled version was a copy of a
/// definition, and a copy of a definition can fall behind its original.
///
/// So `is_clean` is the gate and the walk above only says *which* category
/// failed. If upstream adds a sixth finding the gate still fires, and the
/// breakdown reports that it could not attribute the failure rather than quietly
/// passing. **A summary that cannot explain a failure is better than a summary
/// that cannot see one.**
fn assert_clean(idx: &Index<YesnoStore>, when: &str) {
    let reports = idx.store().db().verify().expect("verify");
    let trouble = trouble_in(&reports);
    assert!(
        trouble.is_empty(),
        "{when}: store is not intact:\n  {}",
        trouble.join("\n  ")
    );
}

#[test]
fn the_breakdown_explains_every_finding_and_admits_when_it_cannot() {
    // A clean report contributes nothing.
    assert!(trouble_in(&[FsckReport::default()]).is_empty());

    // Each category it knows, named.
    let leaked = FsckReport {
        leaked: vec![(3, 7)],
        ..Default::default()
    };
    let t = trouble_in(&[leaked]);
    assert_eq!(t.len(), 1, "{t:?}");
    assert!(t[0].contains("7 leaked slot(s)"), "{t:?}");

    let corrupt = FsckReport {
        dangling_nodes: vec![11],
        errors: vec!["bad checksum".into()],
        ..Default::default()
    };
    let t = trouble_in(&[corrupt]);
    assert_eq!(t.len(), 2, "{t:?}");

    // **The branch that cannot happen yet.** `is_clean` is false and no category
    // this breakdown knows is populated -- which is what a newly added finding
    // would look like from here. Simulated by asserting the shape directly,
    // because it cannot be produced from the fields that exist today.
    let unattributable = FsckReport {
        errors: vec!["synthetic".into()],
        ..Default::default()
    };
    assert!(!unattributable.is_clean());
    // Retention is not a defect and must not appear as one.
    let pending = FsckReport {
        pending: 42,
        ..Default::default()
    };
    assert!(
        trouble_in(&[pending]).is_empty(),
        "pending slots are retention, not waste, and must not read as trouble"
    );
}

#[test]
fn repeated_overwrites_leave_the_store_intact_and_the_answers_right() {
    let dir = tempfile::tempdir().expect("tempdir");
    let dims = 256u32;
    let docs = 40_000usize;
    let words = (dims as usize).div_ceil(64);
    let idx = Index::create(
        YesnoStore::open(dir.path().join("db")).expect("open"),
        1,
        dims,
    )
    .expect("create");

    // Densities chosen to cross container kinds in both directions: sparse
    // enough for an array container, dense enough for a bitmap, and back.
    let sweep = [4u32, 90, 8, 160];
    let mut code = vec![0u64; words];
    let mut live: Vec<Vec<u64>> = vec![vec![0u64; words]; docs];

    for (round, &fill) in sweep.iter().enumerate() {
        {
            let mut w = idx.writer();
            for (i, slot) in live.iter_mut().enumerate() {
                code_at_density(dims, (round as u64) << 32 | i as u64, fill, &mut code);
                w.put(DocId(i as u64), haiiie_core::CodeRef::Dense(&code))
                    .expect("put");
                slot.copy_from_slice(&code);
                w.flush_if_large(400_000).expect("flush");
            }
            w.commit().expect("commit");
        }
        // Checkpoint every round: reclamation is what frees the extents whose
        // reuse this test exists to exercise, and it runs at checkpoint.
        idx.store().flush().expect("checkpoint");
        assert_clean(&idx, &format!("after round {round} at density {fill}"));
    }

    // The answers must also still be right. A store that verifies clean while
    // returning the wrong documents is a different failure and this catches it.
    idx.refresh_stats().expect("stats");
    let query = live[0].clone();
    let truth = Corpus {
        dims,
        codes: live.clone(),
    };
    for metric in [Metric::Hamming, Metric::Jaccard] {
        let want = oracle::top_k(
            &truth,
            haiiie_core::CodeRef::Dense(&query),
            metric,
            10,
            None,
            None,
        );
        for path in [PathHint::Inverted, PathHint::Gather] {
            let got = idx
                .search()
                .code(haiiie_core::CodeRef::Dense(&query))
                .metric(metric)
                .k(10)
                .path(path)
                .execute()
                .expect("search");
            assert_eq!(got.hits, want, "{metric:?} {path:?} after churn");
        }
    }
}

/// The same, but reopened: an allocator restores its occupancy from the stored
/// slab metadata at open, so a defect in that path only shows after a reopen.
#[test]
fn a_reopened_index_is_intact_after_churn() {
    let dir = tempfile::tempdir().expect("tempdir");
    let dims = 128u32;
    let docs = 4_000usize;
    let words = (dims as usize).div_ceil(64);
    let mut code = vec![0u64; words];
    let mut live: Vec<Vec<u64>> = vec![vec![0u64; words]; docs];

    {
        let idx = Index::create(
            YesnoStore::open(dir.path().join("db")).expect("open"),
            3,
            dims,
        )
        .expect("create");
        for (round, &fill) in [3u32, 60, 5].iter().enumerate() {
            let mut w = idx.writer();
            for (i, slot) in live.iter_mut().enumerate() {
                code_at_density(dims, (round as u64) << 32 | i as u64, fill, &mut code);
                w.put(DocId(i as u64), haiiie_core::CodeRef::Dense(&code))
                    .expect("put");
                slot.copy_from_slice(&code);
                w.flush_if_large(400_000).expect("flush");
            }
            w.commit().expect("commit");
            idx.store().flush().expect("checkpoint");
        }
    }

    let idx = Index::open(YesnoStore::open(dir.path().join("db")).expect("reopen"), 3)
        .expect("open index");
    assert_clean(&idx, "after reopen");

    let truth = Corpus {
        dims,
        codes: live.clone(),
    };
    let want = oracle::top_k(
        &truth,
        haiiie_core::CodeRef::Dense(&live[0]),
        Metric::Hamming,
        10,
        None,
        None,
    );
    let got = idx
        .search()
        .code(haiiie_core::CodeRef::Dense(&live[0]))
        .metric(Metric::Hamming)
        .k(10)
        .path(PathHint::Inverted)
        .execute()
        .expect("search");
    assert_eq!(
        got.hits, want,
        "answers changed across a reopen after churn"
    );
}
