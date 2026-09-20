//! The generators produce every container kind the storage engine has.
//!
//! # Why this is a test and not a sentence in a document
//!
//! `TESTING.md` section 4 has said since M0 that generators must be
//! boundary-biased, because uniform random codes never produce an array or a run
//! container and therefore never exercise the paths that handle them. The rule
//! was right, it was cited in conversation, and on 2026-09-15 it failed to stop
//! the person citing it from writing a uniform generator to reproduce a storage
//! bug -- which found nothing, because the bug needed the container diversity a
//! uniform generator cannot produce.
//!
//! The rule was not the problem. **Nothing enforced it**, and every other
//! mechanical rule in this repository exists because the same discovery was made
//! about some other sentence. So this is the enforcement: it asserts the
//! property the rule asks for, against the corpora the suite actually uses.
//!
//! # Its limits, stated rather than implied
//!
//! It pins the **durable** generators in this crate. It cannot pin a throwaway
//! generator written in a scratch instrument, which is exactly where the
//! failure happened, and no plausible check can -- that code is outside the
//! tree by design. What this buys is that the shared corpora cannot quietly
//! become uniform under a later simplification, and that a reader who wants to
//! know what each `Shape` is *for* can read an assertion rather than a promise.

use std::collections::BTreeSet;

use haiiie_core::{DocId, Index, SetStore, YesnoStore};
use haiiie_testkit::{Corpus, Shape};
use yesno_core::Container;
use yesno_core::stream::ChunkStream;

/// Which container kinds a corpus's posting lists occupy once checkpointed.
fn kinds(shape: Shape, dims: u32, docs: usize) -> BTreeSet<&'static str> {
    let dir = tempfile::tempdir().expect("tempdir");
    let c = Corpus::generate(17, dims, docs, shape);
    let store = YesnoStore::open(dir.path().join("db")).expect("open");
    let idx = Index::create(store.clone(), 1, dims).expect("create");
    {
        let mut w = idx.writer();
        for i in 0..c.len() {
            w.put(DocId(i as u64), c.code(i)).expect("put");
        }
        w.commit().expect("commit");
    }
    // Run-optimization happens at checkpoint; without this a contiguous set is
    // still a bitmap and the run arm is never reached.
    store.flush().expect("checkpoint");

    let snap = store.db().snapshot().expect("snapshot");
    let keys = idx.keys();
    let mut seen = BTreeSet::new();
    for d in 0..dims {
        let mut s = snap.key_stream(keys.dim(d)).expect("stream");
        while let Some((_, container)) = s.next_chunk().expect("chunk") {
            seen.insert(match container {
                Container::Array(_) => "array",
                Container::Bitmap(_) => "bitmap",
                Container::Run(_) => "run",
            });
        }
    }
    seen
}

#[test]
fn the_shapes_between_them_produce_array_bitmap_and_run_containers() {
    // Enough documents that a dense dimension exceeds the array/bitmap
    // threshold, and few enough to stay quick.
    let observed: Vec<(Shape, BTreeSet<&str>)> = [
        Shape::Balanced,
        Shape::Sparse,
        Shape::Clustered,
        Shape::Degenerate,
        Shape::OrdinalRuns,
    ]
    .into_iter()
    .map(|s| (s, kinds(s, 128, 20_000)))
    .collect();

    for (shape, seen) in &observed {
        assert!(!seen.is_empty(), "{shape:?} produced no containers at all");
    }

    let all: BTreeSet<&str> = observed
        .iter()
        .flat_map(|(_, s)| s.iter().copied())
        .collect();
    for want in ["array", "bitmap", "run"] {
        assert!(
            all.contains(want),
            "no generator shape produces a {want} container; the corpora have gone \
             uniform and the paths that handle {want} containers are untested.\n\
             observed: {observed:?}"
        );
    }

    // Sparse must be the array case specifically. Without this the assertion
    // above could be satisfied by three shapes drifting into one kind each while
    // `Sparse` silently became dense -- the aggregate would still be complete.
    let sparse = &observed
        .iter()
        .find(|(s, _)| *s == Shape::Sparse)
        .expect("sparse arm")
        .1;
    assert!(
        sparse.contains("array"),
        "Shape::Sparse no longer produces array containers: {sparse:?}"
    );
}
