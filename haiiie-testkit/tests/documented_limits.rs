//! The limits the engine enforces, against the page that states them.
//!
//! # Why this is a test and not a habit
//!
//! Three bounds were added this week -- the largest id, the widest code, the
//! largest attribute term -- and the user-facing page stated **none** of them.
//! Two it actively contradicted, because it described the *type* where the
//! engine accepts a subset: ids as `0` through `2^64 - 2` when `put` refuses
//! above `2^36 - 1`, and terms as a `u32` when a `u32` is sixteen times wider
//! than the key field holds. Describing a term as a `u32` invites hashing a name
//! into it, which is the one scheme that cannot work.
//!
//! The conclusion recorded at the time was that no check was possible, because
//! whether a sentence about a range is still true is a judgement. That was
//! half right and gave up too early. **Whether the page contains the number the
//! engine computes is arithmetic**, and this asserts exactly that.
//!
//! # What it checks, and what it cannot
//!
//! That each limit, rendered the way the page renders numbers, appears
//! somewhere in it. Not that the sentence around it is correct, not that it is
//! the sentence about that limit, and not that the page is complete. A number
//! in an unrelated paragraph would satisfy it.
//!
//! What it does buy is the direction that failed this week: **change a bound and
//! this goes red**, naming the page. The failure a habit was supposed to prevent
//! becomes one the gate reports.
//!
//! Both halves are sabotage-measured rather than asserted. Halving the id bound
//! and leaving the page alone is caught. Reverting the page's contract sentence
//! to the old `2^64 - 2` is **not** -- `2^36 - 1` still appears in the prose
//! explaining it and in the cost example below it, so the page still contains
//! the number. That is the disclaimed boundary behaving as described, and the
//! reason to keep the disclaimer specific: this is a check on a number, not on
//! a sentence, and only one of the two ways it can rot is mechanical.

use haiiie_core::{BLOCK_ORDINALS, Index, KeySpace, YesnoStore};

const PAGE: &str = "../docs/data-modeling.md";

/// How this project writes a number in prose: `2^k - 1` when it is one below a
/// power of two, otherwise digits grouped by a space every three.
fn rendered(v: u64) -> String {
    if (v + 1).is_power_of_two() && v > 1024 {
        return format!("2^{} - 1", (v + 1).trailing_zeros());
    }
    let d = v.to_string();
    let mut out = String::new();
    for (i, c) in d.chars().enumerate() {
        if i > 0 && (d.len() - i).is_multiple_of(3) {
            out.push(' ');
        }
        out.push(c);
    }
    out
}

fn page() -> String {
    std::fs::read_to_string(PAGE).unwrap_or_else(|e| panic!("reading {PAGE}: {e}"))
}

#[test]
fn every_enforced_limit_appears_on_the_page_that_states_the_contract() {
    let dir = tempfile::tempdir().expect("tempdir");
    let idx = Index::create(
        YesnoStore::open(dir.path().join("db")).expect("open"),
        1,
        64,
    )
    .expect("create");

    let limits: [(&str, u64); 3] = [
        ("the largest document id", idx.max_doc_id()),
        (
            "the widest code",
            u64::from(
                u32::try_from(KeySpace::INDEX_MAX + 1)
                    .expect("fits")
                    .min(u32::try_from(BLOCK_ORDINALS).expect("fits")),
            ),
        ),
        ("the largest attribute term", KeySpace::INDEX_MAX),
    ];

    let doc = page();
    for (what, v) in limits {
        let r = rendered(v);
        assert!(
            doc.contains(&r),
            "{what} is {v}, written `{r}`, and {PAGE} does not contain it.\n\
             A bound changed and the page that states the contract did not."
        );
    }
}

/// The renderer is the part that can silently stop matching, so it is pinned
/// separately: a bug here turns this file into a test that always passes.
#[test]
fn the_renderer_matches_how_the_page_writes_numbers() {
    assert_eq!(rendered((1 << 36) - 1), "2^36 - 1");
    assert_eq!(rendered((1 << 20) - 1), "2^20 - 1");
    assert_eq!(rendered(65_536), "65 536");
    assert_eq!(rendered(524_288), "524 288");
    assert_eq!(rendered(10), "10");
    // Below the threshold the exponent form is not used, because small
    // powers-of-two-minus-one are ordinary numbers in prose.
    assert_eq!(rendered(1023), "1 023");
}
