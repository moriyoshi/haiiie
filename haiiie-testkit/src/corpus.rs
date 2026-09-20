//! Corpus generation, biased toward the boundaries that break things.
//!
//! # Uniform random codes test almost nothing
//!
//! This is the lesson yesnodb records for its own generators and it transfers
//! directly. A code drawn uniformly at density 0.5 produces, once inverted, a
//! posting list that is always a bitmap container -- never an array, never a
//! run. So a suite built on uniform codes never executes the array or run paths
//! at all, and reports full confidence having exercised one third of the
//! representation space.
//!
//! The shapes below bias three axes on purpose: **density** ( at both floors and
//! the ceiling, not just the middle ), **clustering** ( contiguous bits, which
//! is what produces run containers ), and **degeneracy** ( all-zero and all-one
//! codes, which are the arguments every count identity divides by ).

use crate::rng::Rng;

/// How a corpus is shaped.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum Shape {
    /// Density near 0.5, bits scattered. Sign-quantized embeddings look like this.
    Balanced,
    /// Density near 1/64, bits scattered. Learned sparse retrieval looks like this.
    Sparse,
    /// Density near 0.5, bits in contiguous runs.
    Clustered,
    /// Mixed degenerate widths, including all-zero and all-one codes.
    Degenerate,
    /// Consecutive **documents** share dimensions, so a posting list is a few
    /// long ordinal ranges rather than a scatter.
    ///
    /// # This is the transpose of `Clustered`, and the distinction is the point
    ///
    /// `Clustered` puts a code's bits in contiguous runs -- contiguous in
    /// *dimension* space, within one document. That produces nothing run-like in
    /// storage, because a posting list holds the **document ordinals** carrying a
    /// bit, and clustering within a code says nothing about which documents are
    /// adjacent.
    ///
    /// Measured: `Balanced`, `Clustered` and `Degenerate` all produce bitmap
    /// containers and `Sparse` produces arrays, so before this variant existed
    /// **no shape produced a run container at all** -- despite `TESTING.md`
    /// section 4 requiring exactly that since M0. The rule named the right
    /// property and nobody had checked which axis it lived on.
    OrdinalRuns,
}

/// A generated corpus of binary codes.
#[derive(Clone, Debug)]
pub struct Corpus {
    /// Bits per code.
    pub dims: u32,
    /// One dense, word-packed code per document.
    pub codes: Vec<Vec<u64>>,
}

impl Corpus {
    /// Generate `n` codes of `dims` bits in the given shape.
    #[must_use]
    pub fn generate(seed: u64, dims: u32, n: usize, shape: Shape) -> Self {
        let mut rng = Rng::new(seed);
        let words = (dims as usize).div_ceil(64);
        let codes = (0..n)
            .map(|i| {
                let mut c = vec![0u64; words];
                match shape {
                    Shape::Balanced => fill_scattered(&mut rng, &mut c, dims, 1, 2),
                    Shape::Sparse => fill_scattered(&mut rng, &mut c, dims, 1, 64),
                    Shape::Clustered => fill_clustered(&mut rng, &mut c, dims),
                    // Every document in a band of 1 024 shares one set of
                    // dimensions, so each of those dimensions' posting lists
                    // gains a contiguous ordinal range of that length.
                    Shape::OrdinalRuns => {
                        let band = (i / 1_024) as u64;
                        let mut r = Rng::new(seed ^ band.wrapping_mul(0x9e37_79b9_7f4a_7c15));
                        fill_scattered(&mut r, &mut c, dims, 1, 2);
                    }
                    Shape::Degenerate => match i % 4 {
                        0 => {}
                        1 => set_all(&mut c, dims),
                        2 => fill_scattered(&mut rng, &mut c, dims, 1, 512),
                        _ => fill_scattered(&mut rng, &mut c, dims, 511, 512),
                    },
                }
                c
            })
            .collect();
        Self { dims, codes }
    }

    /// Borrow one code.
    #[must_use]
    pub fn code(&self, i: usize) -> haiiie_core::CodeRef<'_> {
        haiiie_core::CodeRef::Dense(&self.codes[i])
    }

    /// How many documents.
    #[must_use]
    pub fn len(&self) -> usize {
        self.codes.len()
    }

    /// Whether the corpus is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.codes.is_empty()
    }
}

fn set_bit(c: &mut [u64], b: u32) {
    c[b as usize >> 6] |= 1u64 << (b & 63);
}

fn set_all(c: &mut [u64], dims: u32) {
    for b in 0..dims {
        set_bit(c, b);
    }
}

fn fill_scattered(rng: &mut Rng, c: &mut [u64], dims: u32, num: u32, den: u32) {
    for b in 0..dims {
        if rng.chance(num, den) {
            set_bit(c, b);
        }
    }
}

/// Contiguous runs, which is what makes an inverted posting list a run container.
fn fill_clustered(rng: &mut Rng, c: &mut [u64], dims: u32) {
    let mut b = 0;
    while b < dims {
        let run = 1 + rng.below(u64::from(dims / 8).max(1)) as u32;
        let end = (b + run).min(dims);
        if rng.chance(1, 2) {
            for x in b..end {
                set_bit(c, x);
            }
        }
        b = end;
    }
}
