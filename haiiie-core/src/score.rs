//! Similarities, compared exactly.
//!
//! # No floating point, anywhere in an ordering
//!
//! Every similarity here is a ratio of integers, and every comparison is a
//! cross-multiplication in `u128`. Reducing two ratios to `f64` first would make
//! ties depend on rounding, which makes a top-k boundary depend on rounding, and
//! this crate's whole claim is that its answer is exact and reproducible. Floats
//! appear only where a human reads a number; [`Hit`] additionally carries the raw
//! `(a, w)` pair so a caller can re-derive the score rather than trust ours.
//!
//! # One accumulator serves four metrics
//!
//! With `a = |q AND x|`, `w = |x|` and `m = |q|`, and because `a <= w` always,
//! every supported similarity is bounded above by a **monotone non-decreasing
//! function of `a` alone**:
//!
//! | metric | exact | bound `g(a)` |
//! |---|---|---|
//! | inner product | `a` | `a` |
//! | Hamming, as `-H` | `2a - m - w` | `a - m` |
//! | Jaccard | `a / (m + w - a)` | `a / m` |
//! | cosine | `a / sqrt(m*w)` | `sqrt(a / m)` |
//!
//! So a search accumulates `a` once, takes a provisional top-k, and refines
//! against `g_inv(tau)` -- nothing scoring below that threshold on `a` can reach
//! the running k-th best on the real metric. [`Metric::bound`] and
//! [`Metric::min_intersection`] are the two halves of that, and they must remain
//! exact inverses of each other or the refinement silently drops results.

use crate::code::DocId;

/// An exactly comparable similarity.
///
/// Scores from **different metrics are not comparable**. [`Ord`] is total
/// because the trait requires it, but a cross-variant comparison is a
/// programming error and its result is not meaningful.
#[derive(Clone, Copy, Debug)]
pub enum Score {
    /// An integer similarity, possibly negative ( Hamming ).
    Int(i64),
    /// The value `num / den`.
    Ratio {
        /// Numerator.
        num: u64,
        /// Denominator; never zero.
        den: u64,
    },
    /// The value `sqrt( num / den )`.
    RatioSq {
        /// Numerator.
        num: u128,
        /// Denominator; never zero.
        den: u128,
    },
}

impl Score {
    /// A ratio, normalizing a zero denominator to an honest zero.
    #[must_use]
    pub fn ratio(num: u64, den: u64) -> Self {
        if den == 0 {
            Self::Ratio { num: 0, den: 1 }
        } else {
            Self::Ratio { num, den }
        }
    }

    /// A square-rooted ratio, normalizing a zero denominator to an honest zero.
    #[must_use]
    pub fn ratio_sq(num: u128, den: u128) -> Self {
        if den == 0 {
            Self::RatioSq { num: 0, den: 1 }
        } else {
            Self::RatioSq { num, den }
        }
    }

    /// A lossy rendering, for display only. Never use this in an ordering.
    #[must_use]
    pub fn as_f64(&self) -> f64 {
        match *self {
            Self::Int(v) => v as f64,
            Self::Ratio { num, den } => num as f64 / den as f64,
            Self::RatioSq { num, den } => (num as f64 / den as f64).sqrt(),
        }
    }

    const fn rank(&self) -> u8 {
        match self {
            Self::Int(_) => 0,
            Self::Ratio { .. } => 1,
            Self::RatioSq { .. } => 2,
        }
    }
}

impl Ord for Score {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        match (*self, *other) {
            (Self::Int(a), Self::Int(b)) => a.cmp(&b),
            // a1/d1 vs a2/d2, cross-multiplied. u128 cannot overflow on u64 inputs.
            (Self::Ratio { num: a, den: b }, Self::Ratio { num: c, den: d }) => {
                (u128::from(a) * u128::from(d)).cmp(&(u128::from(c) * u128::from(b)))
            }
            // sqrt is monotone, so comparing the radicands is comparing the roots.
            (Self::RatioSq { num: a, den: b }, Self::RatioSq { num: c, den: d }) => {
                (a.saturating_mul(d)).cmp(&c.saturating_mul(b))
            }
            (x, y) => {
                debug_assert_eq!(
                    x.rank(),
                    y.rank(),
                    "scores from different metrics were compared"
                );
                x.rank().cmp(&y.rank())
            }
        }
    }
}

impl PartialOrd for Score {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// Equality is the **value**, not the representation.
///
/// `PartialEq` was derived while `Ord` cross-multiplies, so the two disagreed:
/// `ratio(1, 4)` and `ratio(2, 8)` are the same score, compare `Equal`, and were
/// not `==`. Nothing here reduces a fraction -- a gcd per scored document is a
/// hot-path cost for a canonical form nobody reads -- so both representations
/// genuinely occur. Jaccard at `m = 4` gives `1/4` for a document of weight 1
/// and `2/8` for one of weight 6.
///
/// `Ord`'s contract requires `a.cmp(b) == Equal` exactly when `a == b`, and a
/// type breaking it answers the same question two ways: a `BTreeSet<Score>`
/// orders by `cmp` and would treat those two as one score, while `==` reports
/// them distinct. `Score` is public and re-exported at the crate root, so that
/// is a consumer's bug to inherit rather than ours to know about.
///
/// The rank guard keeps cross-variant equality `false` without reaching the
/// debug assertion in `cmp`, which exists to catch scores from different metrics
/// being compared and should not fire on an equality test.
impl PartialEq for Score {
    fn eq(&self, other: &Self) -> bool {
        self.rank() == other.rank() && self.cmp(other).is_eq()
    }
}

impl Eq for Score {}

/// Which similarity a search ranks by.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
#[non_exhaustive]
pub enum Metric {
    /// `|q AND x|`.
    Dot,
    /// Hamming distance, ranked as its negation so larger is better.
    #[default]
    Hamming,
    /// `|q AND x| / |q OR x|`.
    Jaccard,
    /// `|q AND x| / sqrt(|q| * |x|)`.
    Cosine,
}

impl Metric {
    /// The exact similarity, from the intersection `a`, the document weight `w`
    /// and the query weight `m`.
    #[must_use]
    pub fn score(self, a: u32, w: u32, m: u32) -> Score {
        let (a64, w64, m64) = (i64::from(a), i64::from(w), i64::from(m));
        match self {
            Self::Dot => Score::Int(a64),
            Self::Hamming => Score::Int(2 * a64 - m64 - w64),
            Self::Jaccard => Score::ratio(u64::from(a), u64::from(m + w - a)),
            Self::Cosine => {
                Score::ratio_sq(u128::from(a) * u128::from(a), u128::from(m) * u128::from(w))
            }
        }
    }

    /// The Hamming distance itself, for callers that want the distance rather
    /// than the ranking. `H = m + w - 2a`.
    #[must_use]
    pub fn hamming_distance(a: u32, w: u32, m: u32) -> u32 {
        m + w - 2 * a
    }

    /// A monotone non-decreasing upper bound on the similarity, from `a` alone.
    ///
    /// Exact-search correctness rests on this being a true upper bound: nothing
    /// with intersection `a` can score above `bound(a)`.
    #[must_use]
    pub fn bound(self, a: u32, m: u32) -> Score {
        match self {
            Self::Dot => Score::Int(i64::from(a)),
            Self::Hamming => Score::Int(i64::from(a) - i64::from(m)),
            Self::Jaccard => Score::ratio(u64::from(a), u64::from(m)),
            Self::Cosine => Score::ratio_sq(u128::from(a), u128::from(m)),
        }
    }

    /// A monotone upper bound that additionally knows every candidate's weight
    /// is at least `w_min`.
    ///
    /// Jaccard is `a/(m+w-a)`, and `w >= w_min` makes the denominator at least
    /// `m + w_min - a`, so `a / max(m, m+w_min-a)` is still a guarantee and is
    /// tighter whenever `w_min > a`. Cosine is `a/sqrt(m*w)` and gains the same
    /// way through `w >= max(a, w_min)`.
    ///
    /// **Measured**: on 524 288 random 128-bit codes the loose Jaccard bound
    /// admits 42.5% of documents, this one admits 20.7%, and with
    /// weight-sorted ordinals -- which make a block's weight range narrow --
    /// **0.1%**. That is the whole return on per-block statistics, and it needs
    /// no centroid and no clustering of codes.
    ///
    /// `w_min = 0` reduces to [`Metric::bound`] exactly.
    #[must_use]
    pub fn bound_with(self, a: u32, m: u32, w_min: u32) -> Score {
        match self {
            Self::Dot | Self::Hamming => self.bound(a, m),
            Self::Jaccard => Score::ratio(u64::from(a), u64::from(m.max(m + w_min - a.min(w_min)))),
            Self::Cosine => Score::ratio_sq(
                u128::from(a) * u128::from(a),
                u128::from(m) * u128::from(a.max(w_min).max(1)),
            ),
        }
    }

    /// The least intersection that could still reach `tau`, given `w_min`.
    ///
    /// # Scanned, not solved
    ///
    /// [`Metric::bound_with`] is monotone non-decreasing in `a`, so its inverse
    /// is well defined -- but its closed form branches on whether `a` has passed
    /// `w_min`, and a case analysis that is subtly wrong here does not fail
    /// loudly: too high a threshold silently drops results. The domain is
    /// `0..=m`, this runs **once per block** rather than per document, and a
    /// linear scan is obviously correct. That trade is the right way round.
    #[must_use]
    pub fn min_intersection_with(self, tau: Score, m: u32, w_min: u32) -> u32 {
        match self {
            Self::Dot | Self::Hamming => self.min_intersection(tau, m),
            _ => (0..=m)
                .find(|&a| self.bound_with(a, m, w_min) >= tau)
                .unwrap_or(m),
        }
    }

    /// The least intersection that could still reach `tau` -- the inverse of
    /// [`Metric::bound`], and the threshold a refinement pass restricts on.
    #[must_use]
    pub fn min_intersection(self, tau: Score, m: u32) -> u32 {
        let ceil_div = |n: u128, d: u128| -> u32 {
            if d == 0 {
                return 0;
            }
            u32::try_from(n.div_ceil(d)).unwrap_or(u32::MAX)
        };
        match (self, tau) {
            (Self::Dot, Score::Int(t)) => u32::try_from(t.max(0)).unwrap_or(u32::MAX),
            (Self::Hamming, Score::Int(t)) => {
                u32::try_from((t + i64::from(m)).max(0)).unwrap_or(u32::MAX)
            }
            (Self::Jaccard, Score::Ratio { num, den }) => {
                ceil_div(u128::from(num) * u128::from(m), u128::from(den))
            }
            (Self::Cosine, Score::RatioSq { num, den }) => ceil_div(num * u128::from(m), den),
            _ => 0,
        }
    }
}

/// One result: the document, its exact score, and the two counts it came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hit {
    /// The document.
    pub id: DocId,
    /// Its exact similarity under the query's metric.
    pub score: Score,
    /// `|q AND x|`.
    pub inter: u32,
    /// `|x|`.
    pub weight: u32,
}

impl Hit {
    /// The total order results are returned in: **score descending, then id
    /// ascending**. Deterministic, and the only ordering this crate promises.
    #[must_use]
    pub fn rank_key(&self) -> (std::cmp::Reverse<Score>, DocId) {
        (std::cmp::Reverse(self.score), self.id)
    }
}
