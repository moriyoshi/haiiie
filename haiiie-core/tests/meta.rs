//! Metadata must survive the round trip through an ordinal set.

use haiiie_core::IndexMeta;

#[test]
fn every_width_round_trips_through_ordinals() {
    // The bug this pins: the encoded header ends in zero bytes for every width
    // whose `rows_per_block` is small, so an encoding that inferred its length
    // from the highest set bit produced a short buffer and failed to decode.
    // Only a reopen could see it, because `create` keeps the value in memory.
    for dims in [1u32, 7, 63, 64, 65, 128, 255, 256, 257, 768, 1024, 4096] {
        let m = IndexMeta::new(dims);
        assert_eq!(
            IndexMeta::decode(&m.encode()).expect("decode"),
            m,
            "dims={dims}"
        );
        let ords = m.to_ordinals();
        assert_eq!(
            IndexMeta::from_ordinals(&ords).expect("ordinals"),
            m,
            "dims={dims}"
        );
    }
}

#[test]
fn a_header_ending_in_zero_bytes_is_the_normal_case() {
    // Guard against the regression being "fixed" by a padding constant: at
    // An index with caller-defined codes has no model identity, so its header
    // ends in 32 zero bytes that only the terminator can preserve.
    let m = IndexMeta::new(256);
    let bytes = m.encode();
    assert_eq!(
        &bytes[bytes.len() - 2..],
        &[0, 0],
        "the fixture no longer covers the bug"
    );
    assert_eq!(IndexMeta::from_ordinals(&m.to_ordinals()).expect("rt"), m);
}

#[test]
fn a_missing_or_damaged_blob_is_refused_rather_than_guessed() {
    assert!(IndexMeta::from_ordinals(&[]).is_err(), "empty");
    assert!(IndexMeta::from_ordinals(&[3]).is_err(), "unterminated");
    // A bit beyond the terminator. Given ascending input the terminator is
    // always the maximum, so this branch is only reachable from unsorted input
    // -- which the signature permits, so it is guarded rather than assumed.
    let base = IndexMeta::new(128).to_ordinals();
    let last = *base.last().expect("sentinel");
    let mut unsorted = vec![last + 8];
    unsorted.extend_from_slice(&base);
    assert!(
        IndexMeta::from_ordinals(&unsorted).is_err(),
        "bit past terminator"
    );

    // Trailing garbage inside the terminator: a longer blob is corruption, not
    // a future field. A future *version* is refused by the version check.
    let mut long = base.clone();
    let s = long.pop().expect("sentinel");
    long.push(s + 8);
    assert!(IndexMeta::from_ordinals(&long).is_err(), "over-long blob");
}

#[test]
fn geometry_is_derived_not_stored_twice() {
    // decode() recomputes and compares, so a blob whose fields disagree with
    // one another is refused instead of producing a plausible wrong layout.
    let mut bytes = IndexMeta::new(256).encode();
    bytes[10] ^= 0xFF; // corrupt row_bits
    assert!(IndexMeta::decode(&bytes).is_err());
}

/// A layout version this build does not write is refused, and refused *as* a
/// layout mismatch rather than as damage.
///
/// The version is still 1 and haiiie has not shipped, so nothing in the world
/// currently trips this. It is tested anyway, because the check is otherwise
/// indistinguishable from dead code and the first change that needs it will be
/// the one that cannot afford it to have been deleted.
///
/// What it will guard is a change of exactly the shape already made once: the
/// forward codes moved from one key per chunk to a single key, and the merged
/// key's number is the number the old scheme used for block zero. An index from
/// the earlier layout would find real data there, read its first
/// `rows_per_block` documents correctly, and return an all-zero row for every
/// document after them -- scoring as a code of weight zero, with no missing key
/// and no decode failure. Loud is the whole point.
///
/// Asserts the **variant**, not `is_err()`: an `is_err()` assertion would
/// survive a later change that folded this back into `CorruptMeta`, which is the
/// failure it exists to prevent rather than the one it appears to test.
#[test]
fn an_unknown_layout_version_is_refused_as_a_mismatch_rather_than_corruption() {
    let known = {
        let b = IndexMeta::new(256).encode();
        u16::from_le_bytes([b[4], b[5]])
    };

    for other in [known.wrapping_sub(1), known + 1, 9999] {
        let mut bytes = IndexMeta::new(256).encode();
        // Bytes 4..6 are the version field.
        bytes[4..6].copy_from_slice(&other.to_le_bytes());

        match IndexMeta::decode(&bytes) {
            Err(haiiie_core::Error::UnsupportedLayout { found, expected }) => {
                assert_eq!(found, other);
                assert_eq!(expected, known);
            }
            o => panic!("version {other}: expected UnsupportedLayout, got {o:?}"),
        }
        // And the message has to say what to do, because there is no in-place
        // upgrade and the holder would otherwise be left guessing.
        let msg = IndexMeta::decode(&bytes).expect_err("refused").to_string();
        assert!(msg.contains("rebuild"), "unhelpful message: {msg}");
    }

    // The version this build writes is of course accepted.
    assert!(IndexMeta::decode(&IndexMeta::new(256).encode()).is_ok());
}

#[test]
fn an_optional_model_identity_round_trips_and_has_a_presence_bit() {
    let identity = [0xa5; 32];
    let bound = IndexMeta::with_model_id(512, identity);
    assert_eq!(IndexMeta::decode(&bound.encode()).expect("decode"), bound);
    assert_eq!(
        IndexMeta::from_ordinals(&bound.to_ordinals()).expect("ordinal round trip"),
        bound
    );

    let mut absent_with_payload = IndexMeta::new(512).encode();
    absent_with_payload[19] = 1;
    assert!(
        IndexMeta::decode(&absent_with_payload).is_err(),
        "an absent identity accepted nonzero identity bytes"
    );
    let mut unknown_presence = bound.encode();
    unknown_presence[18] = 2;
    assert!(
        IndexMeta::decode(&unknown_presence).is_err(),
        "an unknown model-identity presence value was accepted"
    );
}
