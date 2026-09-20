//! Test apparatus for haiiie: the brute-force oracle, boundary-biased corpus
//! generation, and a deterministic RNG.
//!
//! This crate is `publish = false` and exists so that the oracle is a real
//! artifact with its own tests rather than a helper buried in one test file. It
//! is the thing every kernel in `haiiie-core` will be checked against, so it is
//! written for obviousness over speed in every case where the two conflict.

pub mod corpus;
pub mod counting_alloc;
pub mod evicting;
pub mod memstore;
pub mod oracle;
pub mod rng;

pub use corpus::{Corpus, Shape};
pub use evicting::EvictingStore;
pub use memstore::MemStore;
pub use rng::Rng;
