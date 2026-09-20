//! A seeded SplitMix64, so a corpus is reproducible from a `u64`.
//!
//! Hand-rolled rather than pulled in: it is nine lines, it removes a dependency
//! from a crate every test links, and -- the part that matters -- a failing
//! property test is reproducible from the seed printed in its output without
//! anyone matching a crate version.

/// A deterministic value source.
#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    /// Seed the generator.
    #[must_use]
    pub const fn new(seed: u64) -> Self {
        Self(seed)
    }

    /// The next 64 bits.
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A value in `[0, n)`. Biased by at most `2^-64 * n`, which no corpus here
    /// is large enough to notice.
    pub fn below(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { self.next_u64() % n }
    }

    /// True with probability `num / den`.
    pub fn chance(&mut self, num: u32, den: u32) -> bool {
        self.below(u64::from(den)) < u64::from(num)
    }
}
