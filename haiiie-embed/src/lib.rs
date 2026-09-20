//! Float-to-code support for haiiie's fixed residual model.
//!
//! Layered above `haiiie-core` on purpose. The engine is binary-only and knows
//! nothing about floats. Residual ingest retains one 64-byte integer code per
//! document and no source embedding.

pub mod residual;
pub mod rng;

pub use residual::{
    CodecError, MAX_NORM_CODE, ModelId, NORM_BITS, PACKED_CODE_BYTES, PackedCode, PreparedQuery,
    RESIDUAL_SIGNS, ResidualHit, ResidualModel,
};
pub use rng::Rng;
