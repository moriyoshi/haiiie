//! Keys must stay distinguishable through a persisted B+tree leaf.
//!
//! # Why this test exists and what it is aimed at
//!
//! A yesnodb chunk key is `(key << 48) | prefix`, so a leaf's truncated suffix
//! of `s` bytes covers the low `8s` bits. At the widths haiiie's layout provokes
//! -- 10 and 12 -- a suffix comparison narrowed to 64 bits would discard chunk
//! bits `64..8s`, which are **user-key bits 16..32** ( width 10 ) and
//! `16..48` ( width 12 ).
//!
//! haiiie's layout is `(namespace << 56) | (kind << 20) | index`, so two keys of
//! different kinds and equal index differ **only** at bits 20..28 -- entirely
//! inside that discarded range. `live()` and `dim(0)` are exactly such a pair
//! and both exist in every index. Under a narrowed comparison they are
//! indistinguishable, and a lookup for one can return the other's ordinals.
//!
//! Upstream fixed precisely this ( the silent half of a crash we reported; the
//! crash was the loud half ) and warned that a suite can cover the crash while
//! missing the truncation, because keys small enough to be unchanged by
//! truncation still pass. This pins the distinguishing property directly rather
//! than relying on it falling out of the scoring tests.

use haiiie_core::{KeySpace, SetSnapshot, SetStore, YesnoStore, store::Batch};

/// The bits a 64-bit-narrowed suffix comparison would discard, for a chunk key
/// built as `(key << 48) | prefix`.
const DISCARDED: std::ops::Range<u32> = 16..32;

fn differing_bits(a: u64, b: u64) -> std::ops::Range<u32> {
    let d = a ^ b;
    assert_ne!(d, 0, "keys must differ");
    d.trailing_zeros()..(64 - d.leading_zeros())
}

#[test]
fn haiiie_keys_differ_only_where_a_narrowed_comparison_would_not_look() {
    // Not an assertion about correctness -- an assertion that this test is
    // aimed at the right thing. If the key layout ever changes so that pairs
    // differ below bit 16, the round-trip below stops covering the truncation
    // case and this fails to say so.
    let k = KeySpace::new(3);
    for (an, a, bn, b) in [
        ("live", k.live(), "dim(0)", k.dim(0)),
        ("dim(0)", k.dim(0), "zplane(0)", k.zplane(0)),
        ("meta", k.meta(), "forward", k.forward()),
    ] {
        let r = differing_bits(a, b);
        assert!(
            r.start >= DISCARDED.start && r.end <= DISCARDED.end,
            "{an} ^ {bn} differs in bits {r:?}, outside the discarded range {DISCARDED:?}; \
             this test no longer covers a narrowed suffix comparison"
        );
    }
}

#[test]
fn keys_differing_only_in_the_discarded_range_survive_a_reopen() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("db");
    let k = KeySpace::new(3);

    // Distinct content per key, and enough ordinals each that the ChunkRef is an
    // out-of-line array rather than inline -- an inline ref never reaches the
    // suffix comparison at all, so a smaller fixture would not test it.
    let cases: [(u64, u64); 4] = [
        (k.live(), 100),
        (k.dim(0), 200),
        (k.zplane(0), 300),
        (k.forward(), 400),
    ];
    {
        let store = YesnoStore::open(&path).expect("open");
        let mut b = Batch::new();
        for (key, base) in cases {
            for o in 0..8u64 {
                b.insert(key, base + o);
            }
        }
        store.write(&b).expect("write");
        store.flush().expect("checkpoint");
    }

    // Reopen, so the read goes through the persisted tree rather than a memtable
    // that never consults a suffix at all.
    let store = YesnoStore::open(&path).expect("reopen");
    let snap = store.snapshot().expect("snapshot");
    for (key, base) in cases {
        let got = snap.load(key).expect("load");
        let want: Vec<u64> = (0..8).map(|o| base + o).collect();
        assert_eq!(got, want, "key {key:#x} returned another key's ordinals");
    }
}

// The index field's width was documented and unenforced.
//
// `KeySpace::INDEX_MAX` says the largest index any kind can address is
// `2^20 - 1`. It had **no reference anywhere in the workspace** -- found by
// auditing public items for callers -- and nothing bounded the one index a
// caller chooses. `dims` is a caller-supplied `u32` and `Index::create` checked
// only that it was non-zero, so dimension 1 048 576 produced
// `(0x10 << 20) | (1 << 20)`, which is `0x11 << 20`, which is z-plane 0 exactly:
// a posting list and a weight plane sharing one key, and a wrong score rather
// than an error.

/// Every kind's key at the largest legal index is still distinct from every
/// other kind's key at zero.
///
/// This is the property the index field's width exists to provide, stated
/// directly rather than inferred from the shift amounts.
#[test]
fn a_maximal_index_never_reaches_the_kind_field() {
    let k = haiiie_core::KeySpace::new(3);
    let max = u32::try_from(haiiie_core::KeySpace::INDEX_MAX).expect("fits u32");
    let at_max = [
        k.dim(max),
        k.zplane(max),
        k.attr(max),
        k.stat(u64::from(max)),
    ];
    let at_zero = [
        k.meta(),
        k.live(),
        k.dim(0),
        k.zplane(0),
        k.forward(),
        k.stat(0),
        k.attr(0),
    ];
    for a in at_max {
        for z in at_zero {
            assert_ne!(a, z, "an index at INDEX_MAX collided with another kind");
        }
    }
}

#[test]
fn more_dimensions_than_the_key_space_can_address_are_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    // The widest index this layout allows: the minimum of the key field's 2^20
    // and a block's 65 536 ordinals, which is the second.
    let max = u32::try_from(haiiie_core::BLOCK_ORDINALS).expect("fits u32");

    // The accepted neighbour, and it must **work**, not merely be creatable.
    //
    // This originally asserted only that `create` returned `Ok`, and the bound
    // it checked was the key field's 2^20. Every width from 65 537 up to that
    // was creatable and unusable: a row is `row_bits` ordinals inside one
    // 65 536-ordinal block, so `rows_per_block` was **zero** and the first `put`
    // divided by it. Putting a document here is what turns the neighbour from a
    // claim about one function into a claim about the index.
    {
        let s = haiiie_core::YesnoStore::open(dir.path().join("ok")).expect("open");
        let idx = haiiie_core::Index::create(s, 1, max).expect("the widest index");
        assert!(
            idx.meta().rows_per_block >= 1,
            "an index that cannot hold one row per block was accepted"
        );
        let mut w = idx.writer();
        let code = vec![0u64; (idx.meta().row_bits / 64) as usize];
        w.put(haiiie_core::DocId(0), haiiie_core::CodeRef::Dense(&code))
            .expect("the widest index must hold a document");
        w.commit().expect("commit");
    }

    let s = haiiie_core::YesnoStore::open(dir.path().join("over")).expect("open");
    match haiiie_core::Index::create(s, 1, max + 1) {
        Err(haiiie_core::Error::TooManyDimensions { dims, max: m }) => {
            assert_eq!(dims, max + 1);
            assert_eq!(m, max);
        }
        other => panic!(
            "one dimension too many must be refused, got {:?}",
            other.map(|_| ())
        ),
    }
}

// A haiiie Block **is** a yesnodb chunk, and that is an identity rather than a
// coincidence of two constants agreeing.
//
// `block_of` produces a number handed straight to a chunk `seek` as a prefix, so
// if the two ever diverged the scan would not lose an optimization, it would
// read the wrong chunk. The constants are derived from the storage layer's
// `CHUNK_CARD` and `CHUNK_BITS` now; before they were a literal `1 << 16` and a
// literal `>> 16` beside a comment asserting the alignment held "by
// construction".
//
// Derivation makes them agree. This asserts that agreeing is the right thing:
// that haiiie's block number is the storage layer's prefix for the same ordinal,
// and that a block's span is exactly a chunk's.

/// haiiie's block of an ordinal is yesnodb's chunk prefix of that ordinal.
#[test]
fn a_block_number_is_the_chunk_prefix_it_is_used_as() {
    let probes = [
        0u64,
        1,
        65_535,
        65_536,
        65_537,
        131_071,
        131_072,
        1 << 20,
        (1 << 36) - 1,
        u64::MAX - 1,
    ];
    for o in probes {
        let (prefix, low) = yesno_core::split(o);
        assert_eq!(
            haiiie_core::block_of(o),
            prefix,
            "ordinal {o}: block and chunk prefix disagree"
        );
        // And the offset within the block is the offset within the chunk, which
        // is what makes a block mask indexable by `ordinal % BLOCK_ORDINALS`.
        assert_eq!(
            o % haiiie_core::BLOCK_ORDINALS,
            u64::from(low),
            "ordinal {o}: block offset and chunk offset disagree"
        );
        // Round-tripping through the storage layer's own join must land back.
        assert_eq!(yesno_core::join(prefix, low), o);
    }
}

/// A block spans exactly one chunk's worth of ordinals.
#[test]
fn a_block_spans_exactly_one_chunk() {
    assert_eq!(
        haiiie_core::BLOCK_ORDINALS,
        u64::from(yesno_core::CHUNK_CARD)
    );
    // The first ordinal of block `b` is the first ordinal of chunk `b`.
    for b in [0u64, 1, 7, 1 << 20] {
        assert_eq!(b * haiiie_core::BLOCK_ORDINALS, yesno_core::chunk_base(b));
    }
}

/// The ordinal ceiling is the storage layer's, and the id ceiling is ours.
///
/// Three constants describe "how large may a number be" here and they are not
/// the same number. Keeping them straight is what the reserved-id error and the
/// too-large-id error are for, and conflating them is what made
/// `docs/data-modeling.md` publish a range 268 million times too wide.
#[test]
fn the_three_ceilings_are_ordered_and_distinct() {
    let dir = tempfile::tempdir().expect("tempdir");
    let idx = haiiie_core::Index::create(
        haiiie_core::YesnoStore::open(dir.path().join("db")).expect("open"),
        1,
        64,
    )
    .expect("create");

    // The storage layer's, re-exported rather than restated.
    assert_eq!(haiiie_core::ORDINAL_MAX, yesno_core::ORDINAL_MAX);
    // Ours is strictly tighter, and by a lot -- the difference is the whole
    // reason a separate error variant exists for it.
    assert!(
        idx.max_doc_id() < haiiie_core::ORDINAL_MAX,
        "the index's id ceiling must be inside the storage layer's"
    );
    // And the block a permitted id lands in fits the key's index field, which is
    // the constraint that makes ours the tighter one.
    assert!(haiiie_core::block_of(idx.max_doc_id()) <= haiiie_core::KeySpace::INDEX_MAX);
}
