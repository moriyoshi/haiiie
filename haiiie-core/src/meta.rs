//! Index metadata, stored **inside** the set store rather than beside it.
//!
//! # Why a bit-packed blob and not a side file
//!
//! yesnodb stores no values, so the obvious move is a small JSON file next to
//! the data directory. That would be a second thing to keep consistent across a
//! crash -- precisely the failure the write-ahead log, the copy-on-write pages
//! and the atomic multi-key commit exist to prevent. Encoding the blob as the
//! ordinal set of its own set bits costs nothing in space ( a bitmap container
//! is exactly one bit per bit ) and inherits durability, MVCC, backup and
//! replication from the substrate for free.
//!
//! The encoding is deliberately boring and self-describing: a magic, a version,
//! then fixed little-endian fields. It is read once at open.

use crate::{Error, Result};

const MAGIC: u32 = 0x6861_6969; // "haii"

/// The on-disk layout version.
///
/// **Still 1: haiiie has not shipped, but development indexes exist.** The
/// forward codes previously moved from one key per 65 536-ordinal chunk to a
/// single key without a bump. That in-place change is historical, not permission
/// to reinterpret existing data again. A future FWD split must bump this version
/// or use a disjoint kind and must reject an old single-key index on reopen.
///
/// What the check below is *for*, so that it is not mistaken for dead code
/// before it has ever rejected anything: the forward-key merge is exactly the
/// shape of change that would be **silent** rather than loud. The merged key's
/// number is the number the old scheme used for block zero, so an index from the
/// earlier layout would find real data there, read its first `rows_per_block`
/// documents correctly, and return an all-zero row for every document after
/// them -- a code of weight zero, scored without complaint. No missing key, no
/// decode failure, no gap in the output. Once there is a release to break, this
/// check is the only thing between such an index and a result that looks
/// entirely normal and is wrong over most of the corpus. It works only if
/// META remains findable: widening the INDEX field must pin META to its
/// current numeric key and probe the legacy location during creation.
const VERSION: u16 = 1;

/// What an index needs to know about itself.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct IndexMeta {
    /// Bits per code.
    pub dims: u32,
    /// Bits per code rounded up to a word, which is the forward row stride.
    pub row_bits: u32,
    /// Documents per forward block.
    pub rows_per_block: u32,
    /// Opaque identity of the encoder/decoder model, when rows require one.
    pub model_id: Option<[u8; 32]>,
}

impl IndexMeta {
    /// Derive the geometry from a code width.
    ///
    /// `row_bits` is `dims` rounded up to 64 so a row never straddles a word,
    /// and `rows_per_block` is how many such rows fit in one 65 536-ordinal
    /// chunk -- so a forward row never straddles a chunk boundary either. Both
    /// are the "seam at the boundary" rule the substrate's own lenses follow.
    #[must_use]
    pub fn new(dims: u32) -> Self {
        let row_bits = dims.next_multiple_of(64).max(64);
        Self {
            dims,
            row_bits,
            rows_per_block: (crate::keyspace::BLOCK_ORDINALS as u32) / row_bits,
            model_id: None,
        }
    }

    /// Derive geometry and bind every row to one external codec model.
    #[must_use]
    pub fn with_model_id(dims: u32, model_id: [u8; 32]) -> Self {
        Self {
            model_id: Some(model_id),
            ..Self::new(dims)
        }
    }

    /// Whether this model-bound index uses the forward-only residual layout.
    ///
    /// The model-identity presence byte in the persisted header is also the
    /// layout discriminator. Binary indexes keep inverted and statistic views;
    /// residual rows are only interpreted by a matching external model.
    #[must_use]
    pub const fn is_residual(&self) -> bool {
        self.model_id.is_some()
    }

    /// How many bit planes the complement weight `z = D - |x|` needs.
    ///
    /// Derived, never stored: `z` ranges over `[0, dims]`, so a stored copy
    /// would be a second source of truth that could disagree with `dims`.
    #[must_use]
    pub fn z_planes(&self) -> u32 {
        u32::BITS - self.dims.leading_zeros()
    }

    /// Serialize to bytes.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(51);
        v.extend_from_slice(&MAGIC.to_le_bytes());
        v.extend_from_slice(&VERSION.to_le_bytes());
        v.extend_from_slice(&self.dims.to_le_bytes());
        v.extend_from_slice(&self.row_bits.to_le_bytes());
        v.extend_from_slice(&self.rows_per_block.to_le_bytes());
        match self.model_id {
            Some(model_id) => {
                v.push(1);
                v.extend_from_slice(&model_id);
            }
            None => {
                v.push(0);
                v.extend_from_slice(&[0; 32]);
            }
        }
        v
    }

    /// Parse bytes written by [`IndexMeta::encode`].
    pub fn decode(b: &[u8]) -> Result<Self> {
        let bad = || Error::CorruptMeta("metadata blob is not a haiiie index header");
        // Exact length, not a minimum: this version knows how long its header
        // is, so trailing bytes are corruption rather than a future field. A
        // longer *version* is caught by the version check below, which is the
        // separate concern.
        if b.len() != 51 {
            return Err(bad());
        }
        let u32_at = |i: usize| u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
        if u32_at(0) != MAGIC {
            return Err(bad());
        }
        // The magic matched, so this *is* a haiiie header; a version mismatch
        // means an older layout, not damage, and says so.
        let found = u16::from_le_bytes([b[4], b[5]]);
        if found != VERSION {
            return Err(Error::UnsupportedLayout {
                found,
                expected: VERSION,
            });
        }
        let model_bytes: [u8; 32] = b[19..51].try_into().expect("32 length-checked bytes");
        let model_id = match b[18] {
            0 if model_bytes == [0; 32] => None,
            1 => Some(model_bytes),
            _ => return Err(Error::CorruptMeta("metadata model identity is malformed")),
        };
        let m = Self {
            dims: u32_at(6),
            row_bits: u32_at(10),
            rows_per_block: u32_at(14),
            model_id,
        };
        let geometry = Self::new(m.dims);
        if m.dims == 0
            || m.row_bits == 0
            || m.rows_per_block == 0
            || m.row_bits != geometry.row_bits
            || m.rows_per_block != geometry.rows_per_block
        {
            return Err(Error::CorruptMeta(
                "metadata geometry is not self-consistent",
            ));
        }
        Ok(m)
    }

    /// The blob as the ordinals of its set bits, plus a terminating sentinel.
    ///
    /// # The sentinel is not decoration
    ///
    /// A set of set-bit positions does not record how long the blob was: a
    /// header ending in zero bytes -- which this one does, since
    /// `rows_per_block` is small and little-endian -- loses them entirely, and
    /// reconstructing the length from the highest set bit yields a *short*
    /// buffer that fails to decode. So one bit is always set at `len * 8`, one
    /// past the final byte, which makes the encoding self-delimiting for any
    /// blob length rather than only for those that happen to end in a set bit.
    ///
    /// Found by the reopen test and by nothing else: `create` keeps the metadata
    /// in memory, so every path except `open` was reading a value that had never
    /// made the round trip.
    #[must_use]
    pub fn to_ordinals(&self) -> Vec<u64> {
        let bytes = self.encode();
        let mut out = Vec::new();
        for (i, byte) in bytes.iter().enumerate() {
            for b in 0..8 {
                if byte >> b & 1 == 1 {
                    out.push((i as u64) * 8 + b);
                }
            }
        }
        out.push((bytes.len() as u64) * 8);
        out
    }

    /// Reconstruct from the ordinals [`IndexMeta::to_ordinals`] produced.
    pub fn from_ordinals(ords: &[u64]) -> Result<Self> {
        let Some(&sentinel) = ords.last() else {
            return Err(Error::CorruptMeta(
                "no metadata stored; is this a haiiie index?",
            ));
        };
        if sentinel % 8 != 0 {
            return Err(Error::CorruptMeta("metadata blob has no valid terminator"));
        }
        let mut bytes = vec![0u8; sentinel as usize / 8];
        for &o in &ords[..ords.len() - 1] {
            let (i, b) = (o as usize / 8, o % 8);
            if i >= bytes.len() {
                return Err(Error::CorruptMeta("metadata bit lies past its terminator"));
            }
            bytes[i] |= 1 << b;
        }
        Self::decode(&bytes)
    }
}
