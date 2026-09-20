//! Documents and the binary codes that describe them.
//!
//! # A code is a set, and that is the whole design
//!
//! A `D`-bit code is the set of positions whose bit is 1. Every similarity this
//! crate supports is a function of two numbers over that set -- the size of the
//! intersection with the query, and the size of the code itself -- which is why
//! [`CodeRef`] exposes exactly [`CodeRef::weight`] and [`CodeRef::intersect`]
//! and nothing else. Anything needing a third quantity is a metric this crate
//! does not have a monotone bound for, and it does not belong here without one.
//!
//! # Two representations, because the regimes are genuinely different
//!
//! [`CodeRef::Dense`] is word-packed, LSB-first -- bit-identical to a Roaring
//! bitmap container and to an Arrow boolean buffer, which is what later makes a
//! chunk handoff a refcount bump rather than a decode. [`CodeRef::Sparse`] is an
//! ascending list of set positions, which is smaller whenever the density is
//! below about 1/32 at `u32` positions. Sign-quantized embeddings are dense at
//! density near 0.5; learned sparse retrieval vectors are sparse by construction.

use crate::{Error, Result};

/// The largest ordinal the storage layer accepts.
///
/// yesnodb reserves `u64::MAX` so a full-universe cardinality stays a `u64`, and
/// **this is re-exported from there rather than restated**: a constant whose doc
/// names another crate's rule is that crate's to define, and a copy of a
/// definition can fall behind its original without anything failing.
///
/// It is **not** the largest usable document id, which is what this used to say.
/// haiiie's own layout is tighter -- a block's statistics are keyed by block
/// number, and that index field is twenty bits -- so ids stop at about `2^36`.
/// [`Index::max_doc_id`](crate::Index::max_doc_id) is the number a caller wants;
/// this one is only the outer bound both must respect.
pub const ORDINAL_MAX: u64 = yesno_core::ORDINAL_MAX;

/// A document identifier. **This is the yesnodb ordinal**, not a translation of
/// one: there is no dictionary, and the application owns the assignment.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct DocId(pub u64);

impl DocId {
    /// Construct a checked id, refusing the reserved ordinal.
    pub fn new(v: u64) -> Result<Self> {
        if v > ORDINAL_MAX {
            return Err(Error::ReservedDocId(v));
        }
        Ok(Self(v))
    }

    /// The underlying ordinal.
    #[inline]
    #[must_use]
    pub fn get(self) -> u64 {
        self.0
    }
}

/// A borrowed binary code.
#[derive(Clone, Copy, Debug)]
pub enum CodeRef<'a> {
    /// Word-packed bits, LSB-first within each word.
    Dense(&'a [u64]),
    /// Ascending, unique set-bit positions.
    Sparse(&'a [u32]),
}

impl CodeRef<'_> {
    /// The number of set bits: `|x|`.
    #[must_use]
    pub fn weight(&self) -> u32 {
        match self {
            Self::Dense(w) => w.iter().map(|x| x.count_ones()).sum(),
            Self::Sparse(p) => u32::try_from(p.len()).unwrap_or(u32::MAX),
        }
    }

    /// The size of the intersection: `|self AND other|`.
    ///
    /// The three arms are specializations of one another and must agree; that
    /// agreement is a property test, not an assumption.
    #[must_use]
    pub fn intersect(self, other: Self) -> u32 {
        match (self, other) {
            (Self::Dense(a), Self::Dense(b)) => a
                .iter()
                .zip(b.iter())
                .map(|(x, y)| (x & y).count_ones())
                .sum(),
            (Self::Sparse(s), Self::Dense(d)) | (Self::Dense(d), Self::Sparse(s)) => {
                s.iter()
                    .filter(|&&p| {
                        let (w, b) = (p as usize >> 6, p & 63);
                        d.get(w).is_some_and(|word| word >> b & 1 == 1)
                    })
                    .count() as u32
            }
            (Self::Sparse(a), Self::Sparse(b)) => {
                let (mut i, mut j, mut n) = (0, 0, 0);
                while i < a.len() && j < b.len() {
                    match a[i].cmp(&b[j]) {
                        std::cmp::Ordering::Less => i += 1,
                        std::cmp::Ordering::Greater => j += 1,
                        std::cmp::Ordering::Equal => {
                            n += 1;
                            i += 1;
                            j += 1;
                        }
                    }
                }
                n
            }
        }
    }
}
