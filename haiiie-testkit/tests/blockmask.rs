//! `load_block` against `load`, on the container shapes that have a word image
//! and the ones that do not.
//!
//! Two methods of one snapshot answering the same question is the cheapest
//! differential test available here, and it is worth having because the failure
//! mode is silent. A block mask that is missing a bit drops a live document from
//! every query touching that block; one with a bit too many scores a deleted
//! document. Neither raises anything.
//!
//! The ranges below are chosen to sit on the boundaries a word-at-a-time range
//! fill gets wrong: a run inside one word, a run that starts or ends exactly on
//! a word edge, a run of length one, and a run covering a whole chunk. They also
//! live in a **non-zero block**, because an off-by-one-block bug is invisible to
//! a corpus that fits in block zero -- this project has already shipped one.

use haiiie_core::{Batch, SetSnapshot, SetStore, YesnoStore, slice};

/// Stores implementing only `read` retain the full visitor contract, including
/// one all-zero visit for absence and no visit when the read fails.
#[test]
fn default_block_visit_preserves_read_semantics() {
    use haiiie_core::{Error, Result, store::Lanes};
    struct ReadOnly;
    impl Lanes for ReadOnly {
        fn read(&mut self, lane: usize, block: u64, out: &mut slice::BlockMask) -> Result<bool> {
            assert_eq!(lane, 0);
            if block == 2 {
                return Err(Error::SnapshotExpired { version: 7 });
            }
            out.fill(0);
            if block == 0 {
                out[1023] = 1 << 63;
            }
            Ok(block == 0)
        }
    }
    let mut cursor = ReadOnly;
    for block in [0, 1, 0, 2] {
        let mut calls = 0;
        let result = cursor.with_block(0, block, &mut |words| {
            calls += 1;
            let mut expected = slice::zero_mask();
            if block == 0 {
                expected[1023] = 1 << 63;
            }
            assert_eq!(words, &expected);
        });
        if block == 2 {
            assert!(matches!(result, Err(Error::SnapshotExpired { version: 7 })));
            assert_eq!(calls, 0);
        } else {
            result.unwrap();
            assert_eq!(calls, 1);
        }
    }
    struct FailSecondLane {
        failed: bool,
    }
    impl Lanes for FailSecondLane {
        fn read(&mut self, lane: usize, _: u64, out: &mut slice::BlockMask) -> Result<bool> {
            out.fill(0);
            if lane == 1 && !self.failed {
                self.failed = true;
                return Err(Error::SnapshotExpired { version: 8 });
            }
            out[0] = if lane == 0 { 1 } else { 2 };
            Ok(true)
        }
    }
    let mut cursor = FailSecondLane { failed: false };
    let mut scratch = Vec::new();
    let mut calls = 0;
    assert!(matches!(
        cursor.with_blocks(2, 0, &mut scratch, &mut |_| calls += 1),
        Err(Error::SnapshotExpired { version: 8 })
    ));
    assert_eq!(calls, 0, "a partial multi-lane read must not visit");
    cursor
        .with_blocks(2, 0, &mut scratch, &mut |view| {
            calls += 1;
            assert_eq!(view.lane(0)[0], 1);
            assert_eq!(view.lane(1)[0], 2);
        })
        .unwrap();
    assert_eq!(calls, 1);
}

const BLOCK: u64 = 3;
const BASE: u64 = BLOCK * 65_536;

/// One key, the ranges written into [`BLOCK`] inclusive, and the container kind
/// the case exists to exercise.
type Case = (u64, Vec<(u32, u32)>, &'static str);

/// The cases, each carrying the kind it is there to reach.
///
/// **The kind is asserted, not assumed.** Every comment here used to name one --
/// "a run", "an array container", "still bitmaps" -- and nothing checked, so the
/// file claimed to cover three expansion arms on the strength of its own prose.
/// A case that quietly became a bitmap would leave the arm it was written for
/// untested and the test would stay green. This project has already found that
/// no corpus shape produced a run container for three milestones.
fn cases() -> Vec<Case> {
    vec![
        (1, vec![(0, 65_535)], "run"),      // the whole chunk, one run
        (2, vec![(5, 9)], "run"),           // inside a single word
        (3, vec![(64, 127)], "run"),        // exactly one word
        (4, vec![(63, 64)], "run"),         // straddling a word edge
        (5, vec![(0, 0)], "run"),           // length one, first bit
        (6, vec![(65_535, 65_535)], "run"), // length one, last bit
        (7, vec![(1, 62)], "run"),          // inside word zero, both ends short
        (8, vec![(100, 200), (4_000, 9_000)], "run"), // several runs
        (9, vec![(0, 63), (65_472, 65_535)], "run"), // both word edges of the chunk
        (
            10,
            (0..300).map(|i| (i * 7, i * 7)).collect(),
            "array", // scattered singletons, no word image
        ),
        (
            11,
            // Dense and irregular enough that neither runs nor an array win:
            // the bitmap arm, which is the zero-copy word path.
            (0..20_000)
                .map(|i| (i * 3 % 65_536, i * 3 % 65_536))
                .collect(),
            "bitmap",
        ),
        // The same three boundaries as keys 4 to 6, forced into a **run**
        // container by pairing each with a long run in the same chunk. A
        // one-ordinal interval is where `fill_range`'s `lo == hi` arithmetic
        // shows, and at bit 0 and bit 65 535 it is where its word indexing does.
        // Without these the run path had no single-ordinal interval and no
        // interval touching either end of the chunk.
        (12, vec![(0, 0), (1_000, 40_000)], "run"),
        (13, vec![(63, 64), (1_000, 40_000)], "run"),
        (14, vec![(1_000, 40_000), (65_535, 65_535)], "run"),
    ]
}

/// The container kind stored under `key` at [`BLOCK`], read through the storage
/// layer rather than inferred from how it was written.
fn kind_of(store: &YesnoStore, key: u64) -> &'static str {
    use yesno_core::Container;
    use yesno_core::stream::ChunkStream;

    let snap = store.db().snapshot().expect("snapshot");
    let mut s = snap.key_stream(key).expect("stream");
    s.seek(BLOCK).expect("seek");
    let (prefix, c) = s.next_chunk().expect("chunk").expect("a chunk at BLOCK");
    assert_eq!(prefix, BLOCK, "key {key}: wrong chunk");
    match c {
        Container::Array(_) => "array",
        Container::Bitmap(_) => "bitmap",
        Container::Run(_) => "run",
        // No catch-all deliberately. A variant added upstream should stop this
        // compiling rather than report "other" and let a case silently claim to
        // exercise an arm that no longer exists.
    }
}

#[test]
fn block_mask_matches_the_ordinal_list() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = YesnoStore::open(dir.path().join("db")).expect("open");

    let mut batch = Batch::new();
    for (key, ranges, _) in cases() {
        for (lo, hi) in ranges {
            for v in lo..=hi {
                batch.insert(key, BASE + v as u64);
            }
        }
    }
    store.write(&batch).expect("write");
    // Run-optimization happens at checkpoint, so without this the contiguous
    // ranges above are still bitmaps and the run path is never exercised.
    store.flush().expect("flush");

    let snap = store.snapshot().expect("snapshot");
    let mut mask = slice::zero_mask();
    for (key, _, want_kind) in cases() {
        eprintln!(
            "KIND key {key}: want {want_kind}, is {}",
            kind_of(&store, key)
        );
        let want: Vec<u64> = snap.load(key).expect("load");
        assert!(!want.is_empty(), "key {key} stored nothing");

        let any = snap.load_block(key, BLOCK, &mut mask).expect("load_block");
        assert!(any, "key {key} reported an empty block");

        let got: Vec<u64> = (0..65_536u64)
            .filter(|b| mask[(*b >> 6) as usize] >> (b & 63) & 1 == 1)
            .map(|b| BASE + b)
            .collect();
        // Reported as the first disagreement rather than through `assert_eq!`
        // on the vectors: a whole-chunk run differing in one bit would print
        // 65 536 ordinals twice, which buries the one that matters.
        if got != want {
            let at = got
                .iter()
                .zip(&want)
                .position(|(a, b)| a != b)
                .unwrap_or(got.len().min(want.len()));
            panic!(
                "key {key}: {} ordinals, want {}; first difference at index {at}: \
                 got {:?}, want {:?}",
                got.len(),
                want.len(),
                got.get(at),
                want.get(at),
            );
        }

        // Every other block is empty, and `out` is fully written regardless.
        for other in [0, BLOCK - 1, BLOCK + 1, 40] {
            let any = snap.load_block(key, other, &mut mask).expect("load_block");
            assert!(!any, "key {key} block {other}");
            assert!(
                mask.iter().all(|w| *w == 0),
                "key {key} block {other} left residue"
            );
        }
    }
}

/// A cursor must agree with `load_block` for **any** order of blocks.
///
/// The scan drives lanes in ascending order and a stream can only move forward,
/// so the interesting case is the one the scan reaches rarely: a block redone
/// after an eviction retry, arriving at a cursor that has already passed it. A
/// cursor that answered "nothing here" would be silently correct-looking and
/// would drop every document in that block.
///
/// This is tested here rather than through the eviction suite because that
/// suite injects into `MemStore`, which has no cursor to get wrong. The
/// invariant belongs to stateful cursors, so it is checked against one.
#[test]
fn lanes_agree_with_load_block_in_any_order() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = YesnoStore::open(dir.path().join("db")).expect("open");

    // Three separated blocks, so a lane has gaps to seek across, and different
    // content per block so a read that returned the wrong block is visible.
    let blocks = [1u64, 3, 7];
    let mut batch = Batch::new();
    for (key, ranges, _) in cases() {
        for (bi, b) in blocks.iter().enumerate() {
            for (lo, hi) in &ranges {
                let shift = (bi as u32) * 11;
                let (lo, hi) = (lo.saturating_add(shift), hi.saturating_add(shift));
                for v in lo..=hi.min(65_535) {
                    batch.insert(key, b * 65_536 + v as u64);
                }
            }
        }
    }
    store.write(&batch).expect("write");
    store.flush().expect("flush");

    let snap = store.snapshot().expect("snapshot");
    let keys: Vec<u64> = cases().iter().map(|(k, _, _)| *k).collect();
    let mut lanes = snap
        .open_lanes(&keys)
        .expect("open_lanes")
        .expect("the embedded store offers a cursor");
    let mut borrowed = snap.open_lanes(&keys).unwrap().unwrap();
    let mut all = snap.open_lanes(&keys).unwrap().unwrap();
    let mut all_scratch = Vec::new();

    // Ascending, then descending, then the same block twice, then blocks that
    // hold nothing.
    //
    // The pairs `0, 1` and `2, 3` are the ones that matter and they are here
    // deliberately: reading an empty block leaves the stream parked on the next
    // populated one, and asking for *that* block immediately afterwards is the
    // case where a cursor tracking its position off by one reports a block full
    // of documents as empty. Every other order reopens and hides it.
    // The leading `1, 9, 3` is the third case: seeking past the last chunk
    // exhausts the stream, and a cursor that does not record how far it went
    // then answers a later populated block -- 3 here -- from a spent stream and
    // calls it empty.
    let order: Vec<u64> = [1, 9, 3]
        .into_iter()
        .chain(blocks.iter().copied())
        .chain(blocks.iter().rev().copied())
        .chain([3, 3, 0, 1, 2, 3, 4, 7, 9, 1])
        .collect();

    let (mut a, mut b) = (slice::zero_mask(), slice::zero_mask());
    for block in order {
        for (lane, &key) in keys.iter().enumerate() {
            let want = snap.load_block(key, block, &mut a).expect("load_block");
            let got = lanes.read(lane, block, &mut b).expect("read");
            let mut calls = 0;
            borrowed
                .with_block(lane, block, &mut |words| {
                    calls += 1;
                    assert_eq!(words, &a, "borrowed key {key} block {block}");
                })
                .expect("with_block");
            assert_eq!(calls, 1, "absent blocks must still call the visitor");
            assert_eq!(got, want, "key {key} block {block}: emptiness disagrees");
            if a != b {
                let at = a.iter().zip(&b).position(|(x, y)| x != y).expect("differs");
                panic!(
                    "key {key} block {block}: word {at} is {:#018x} through the cursor, \
                     {:#018x} through load_block",
                    b[at], a[at],
                );
            }
        }
        let mut calls = 0;
        all.with_blocks(keys.len(), block, &mut all_scratch, &mut |view| {
            calls += 1;
            assert_eq!(view.len(), keys.len());
            for (lane, &key) in keys.iter().enumerate() {
                snap.load_block(key, block, &mut a).expect("load_block");
                if view.lane(lane) != &a {
                    let word = view
                        .lane(lane)
                        .iter()
                        .zip(&a)
                        .position(|(x, y)| x != y)
                        .unwrap();
                    panic!(
                        "all-lane key {key} block {block} word {word}: got {:#018x}, want {:#018x}",
                        view.lane(lane)[word],
                        a[word]
                    );
                }
            }
        })
        .expect("with_blocks");
        assert_eq!(calls, 1, "all lanes must be borrowed together");
    }
}

/// Simultaneous persisted visits must alias the same mmap bitmap, not copies.
/// Pointer identity distinguishes borrowing from copying even when every bit
/// and every allocation budget would otherwise agree.
#[test]
fn bitmap_visits_borrow_the_existing_words() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let store = YesnoStore::open(&path).unwrap();
    let mut batch = Batch::new();
    for i in (0..65_536u64).step_by(3) {
        batch.insert(1, BASE + i);
    }
    store.write(&batch).unwrap();
    let check = |store: &YesnoStore, persisted: bool| {
        assert_eq!(kind_of(store, 1), "bitmap");
        let snap = store.snapshot().unwrap();
        let mut lanes = snap.open_lanes(&[1]).unwrap().unwrap();
        let mut second = snap.open_lanes(&[1]).unwrap().unwrap();
        let mut called = false;
        lanes
            .with_block(0, BLOCK, &mut |borrowed| {
                called = true;
                second
                    .with_block(0, BLOCK, &mut |other| {
                        assert_eq!(borrowed, other);
                        // Memtable streams may own distinct materializations.
                        // Persisted streams share the same mmap backing.
                        if persisted {
                            assert_eq!(borrowed.as_ptr(), other.as_ptr(), "bitmap was copied");
                        }
                    })
                    .unwrap();
            })
            .unwrap();
        assert!(called);
        let mut all = snap.open_lanes(&[1]).unwrap().unwrap();
        let mut single = snap.open_lanes(&[1]).unwrap().unwrap();
        let mut scratch = Vec::new();
        all.with_blocks(1, BLOCK, &mut scratch, &mut |view| {
            single
                .with_block(0, BLOCK, &mut |words| {
                    assert_eq!(view.lane(0), words);
                    if persisted {
                        assert_eq!(
                            view.lane(0).as_ptr(),
                            words.as_ptr(),
                            "all-lane bitmap was copied"
                        );
                    }
                })
                .unwrap();
        })
        .unwrap();
    };
    check(&store, false);
    store.flush().unwrap();
    drop(store);
    check(&YesnoStore::open(&path).unwrap(), true);
}
