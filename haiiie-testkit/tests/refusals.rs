//! The refusals: every error this crate raises on its own behalf.
//!
//! # Why this file exists
//!
//! An audit of which error variants the whole suite ever constructs found
//! **five of eight never constructed**. Three of those are plain public API
//! refusals reachable in one call -- a reserved id, a zero-dimension index, a
//! namespace collision, an over-wide code -- and nothing anywhere had ever
//! produced one. The paths that compute an answer are covered exhaustively by
//! differential tests; the paths that decline to produce one were covered only
//! where someone thought of them, and here nobody had.
//!
//! The remaining two are `Io` and `Store`, which wrap a foreign error rather
//! than expressing a decision of ours.
//!
//! **That was first written as "reachable only by making the filesystem or the
//! storage engine fail", and it was wrong about `Io`.** `haiiie-core` performs
//! no IO of its own, which is true of this crate and not of the workspace:
//! `haiiie-embed`'s float store calls `File::open` in a function returning this
//! crate's `Result`, so a path that does not exist converts straight into `Io`.
//! Nothing was failing; the file was simply absent. It is covered there, beside
//! the same path once it exists.
//!
//! `Store` is the one that genuinely needs the storage engine to fail, and it
//! is deliberately not tested here. The distinction worth carrying: a variant
//! with no producer in one crate can have an ordinary producer one crate over,
//! so the scope of the audit is part of its result.
//!
//! # Each refusal is paired with the case that must be accepted
//!
//! A test that only asserts the rejection passes just as well against a
//! function that rejects everything. The accepted neighbour is what makes the
//! boundary a boundary rather than a wall, so every test below names the
//! largest accepted value beside the smallest rejected one.

use haiiie_core::{CodeRef, DocId, Error, Index, YesnoStore};

const DIMS: u32 = 256;

fn store(dir: &tempfile::TempDir, name: &str) -> YesnoStore {
    YesnoStore::open(dir.path().join(name)).expect("open")
}

#[test]
fn the_reserved_ordinal_is_refused_and_the_one_below_it_is_not() {
    let max = DocId::new(u64::MAX - 1).expect("the largest usable ordinal");
    assert_eq!(max.get(), u64::MAX - 1);

    match DocId::new(u64::MAX) {
        Err(Error::ReservedDocId(v)) => assert_eq!(v, u64::MAX),
        other => panic!("u64::MAX must be refused, got {other:?}"),
    }
}

#[test]
fn a_zero_dimension_index_is_refused_and_a_one_bit_one_is_not() {
    let dir = tempfile::tempdir().expect("tempdir");
    match Index::create(store(&dir, "zero"), 1, 0) {
        Err(Error::DimensionMismatch { want, got }) => assert_eq!((want, got), (1, 0)),
        other => panic!(
            "zero dimensions must be refused, got {:?}",
            other.map(|_| ())
        ),
    }
    Index::create(store(&dir, "one"), 1, 1).expect("one dimension is a valid index");
}

#[test]
fn creating_over_an_existing_namespace_is_refused_but_a_free_one_is_not() {
    let dir = tempfile::tempdir().expect("tempdir");
    // Dropped before reopening: `Db::open` holds an exclusive directory lock,
    // so this is also the realistic shape -- the collision is discovered by a
    // later process reading persisted metadata, not by one holding the index.
    {
        let _first = Index::create(store(&dir, "db"), 7, DIMS).expect("create");
    }

    match Index::create(store(&dir, "db"), 7, DIMS) {
        Err(Error::AlreadyExists(ns)) => assert_eq!(ns, 7),
        other => panic!("namespace 7 is taken, got {:?}", other.map(|_| ())),
    }
    // A different namespace in the same store is untouched by the collision.
    Index::create(store(&dir, "db"), 8, DIMS).expect("namespace 8 is free");
}

#[test]
fn a_code_wider_than_the_index_is_refused_and_an_exact_one_is_not() {
    let dir = tempfile::tempdir().expect("tempdir");
    let idx = Index::create(store(&dir, "db"), 1, DIMS).expect("create");
    let mut w = idx.writer();

    // Exactly the index's width is accepted.
    let exact = vec![0u64; (DIMS / 64) as usize];
    w.put(DocId(0), CodeRef::Dense(&exact))
        .expect("an exact-width code");

    // One word more is refused, and the error names both widths.
    let wide = vec![0u64; (DIMS / 64) as usize + 1];
    match w.put(DocId(1), CodeRef::Dense(&wide)) {
        Err(Error::DimensionMismatch { want, got }) => {
            assert_eq!(want, DIMS);
            assert!(got > DIMS, "the reported width {got} must exceed {DIMS}");
        }
        other => panic!("an over-wide code must be refused, got {other:?}"),
    }

    // A sparse code naming a position past the end is the same refusal by a
    // different route: the width comes from the highest position, not a length.
    match w.put(DocId(2), CodeRef::Sparse(&[DIMS + 1])) {
        Err(Error::DimensionMismatch { .. }) => {}
        other => panic!("an out-of-range sparse position must be refused, got {other:?}"),
    }
}

/// `DocId` is a tuple struct with a public field, so `DocId::new`'s guard is
/// **advisory**: a caller can write `DocId(u64::MAX)` and never go near it.
///
/// This test records what actually happens on that path rather than asserting
/// what ought to. It is here because the audit above showed the refusal had
/// never fired, and a guard nothing is forced through is worth knowing about
/// whichever way the answer comes out.
#[test]
fn the_reserved_ordinal_does_not_reach_the_store_unchecked() {
    let dir = tempfile::tempdir().expect("tempdir");
    let idx = Index::create(store(&dir, "db"), 1, DIMS).expect("create");
    let mut w = idx.writer();
    let c = vec![1u64; (DIMS / 64) as usize];
    let got = w.put(DocId(u64::MAX), CodeRef::Dense(&c));
    assert!(
        got.is_err(),
        "DocId(u64::MAX) bypasses DocId::new, so `put` is the last line of \
         defence and it accepted the reserved ordinal"
    );
}

/// An attribute term outside the key space's index field is refused.
///
/// # Why this is a refusal and not an assertion
///
/// `Writer::attr` packs `term` into the same 20-bit field `dims` occupies, so a
/// term at or above 2^20 carries into the kind field and the attribute lands
/// under a key belonging to no defined kind. Nothing collided, because the kinds
/// it reaches happen to be unused -- luck, not design, and it would break the
/// first time a kind is added there.
///
/// It became urgent when `KeySpace::key` gained a debug assertion for the same
/// field: that turned a silent misplacement into a **panic on caller input** in
/// debug builds, with no way for the builder to report anything, because `attr`
/// returned `&mut Self`. Making it fallible is what lets the assertion stay
/// unreachable from the public API, which is where an assertion belongs.
///
/// Hashing a term name to a `u32` is the obvious way a caller arrives here.
#[test]
fn an_attribute_term_past_the_key_space_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let idx = Index::create(store(&dir, "db"), 1, DIMS).expect("create");
    let mut w = idx.writer();
    let max = u32::try_from(haiiie_core::KeySpace::INDEX_MAX).expect("fits u32");

    // The accepted neighbour: the largest term the field holds.
    w.put(DocId(0), CodeRef::Dense(&vec![0u64; (DIMS / 64) as usize]))
        .expect("put");
    w.attr(DocId(0), max).expect("the largest addressable term");

    match w.attr(DocId(0), max + 1) {
        Err(Error::AttrTermTooLarge { term, max: m }) => {
            assert_eq!(term, max + 1);
            assert_eq!(m, max);
        }
        other => panic!("one term too many must be refused, got {other:?}"),
    }
    // Both halves of the pair refuse the same values, so a caller cannot detach
    // what it could not attach.
    match w.unattr(DocId(0), max + 1) {
        Err(Error::AttrTermTooLarge { .. }) => {}
        other => panic!("unattr must refuse what attr refuses, got {other:?}"),
    }
}

/// Every id this index accepts fits **every** key it will be used to build.
///
/// # Why this is a property and not an example
///
/// `Index::max_doc_id` originally bounded only the forward row's address, which
/// is the constraint a reader thinks of first and is around `2^58`. The binding
/// one is elsewhere: a block's statistics live under a key indexed by the
/// **block number**, and a key's index field is 20 bits, so `block_of(id)` has
/// to fit as well. That is `2^36 - 1`, four million times lower, and the gap
/// between the two was reachable through the public API.
///
/// So the test is not "is the number right" -- a test naming the number would
/// pass against the wrong number if both were written by the same hand on the
/// same afternoon, which is exactly what happened. It asks instead whether the
/// id the index *says* it accepts can be used everywhere ids are used.
#[test]
fn the_largest_accepted_id_fits_every_key_it_reaches() {
    let dir = tempfile::tempdir().expect("tempdir");
    for dims in [64u32, 256, 1024] {
        let idx = Index::create(store(&dir, &format!("db{dims}")), 1, dims).expect("create");
        let max = idx.max_doc_id();

        // The statistics key, which is the constraint that was missed.
        assert!(
            haiiie_core::block_of(max) <= haiiie_core::KeySpace::INDEX_MAX,
            "D={dims}: id {max} lands in block {}, past the key space's index field",
            haiiie_core::block_of(max)
        );
        // The forward row, which is the one that was checked.
        let addr = idx.row_addr(DocId(max));
        assert!(
            addr.base
                .checked_add(u64::from(idx.meta().row_bits))
                .is_some(),
            "D={dims}: id {max} overflows its own forward row address"
        );

        // And one past it is refused rather than silently addressed.
        let mut w = idx.writer();
        let code = vec![0u64; (idx.meta().row_bits / 64) as usize];
        w.put(DocId(max), CodeRef::Dense(&code))
            .expect("the largest accepted id");
        match w.put(DocId(max + 1), CodeRef::Dense(&code)) {
            Err(Error::DocIdTooLarge { .. }) => {}
            other => panic!("D={dims}: one past the bound must be refused, got {other:?}"),
        }
    }
}
