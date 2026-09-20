//! Deterministic values, because an encoder must be reproducible.
//!
//! A random-projection encoder is only usable if the **same** projection is
//! applied to the corpus and to every later query. Storing the matrix would be
//! megabytes; storing the seed is eight bytes and regenerates it exactly. That
//! only works if the generator is fixed, so it is written here rather than
//! taken from a crate whose output could change across a version.

/// SplitMix64.
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

    /// Uniform in `[0, 1)`.
    pub fn next_f32(&mut self) -> f32 {
        // 24 bits of mantissa: the most a f32 can distinguish, and taking the
        // high bits avoids the weak low bits of a linear generator.
        ((self.next_u64() >> 40) as f32) / ((1u32 << 24) as f32)
    }

    /// Standard normal, by Box-Muller.
    ///
    /// The residual fitter initializes each power iteration with an isotropic
    /// Gaussian axis. A uniform cube would bias that starting direction.
    pub fn next_normal(&mut self) -> f32 {
        let u1 = self.next_f32().max(f32::MIN_POSITIVE);
        let u2 = self.next_f32();
        (-2.0 * u1.ln()).sqrt() * (std::f32::consts::TAU * u2).cos()
    }
}
