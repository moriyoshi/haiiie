//! The fixed 64-byte residual codec selected by the offline recall study.
//!
//! A document row carries 500 residual signs and a 12-bit reciprocal-norm code.
//! The first 496 signs occupy bytes 0 through 61, least-significant bit first.
//! The final little-endian `u16` carries signs 496 through 499 in bits 0 through
//! 3 and the norm code in bits 4 through 15. Query scoring is the exact integer
//! product `dot * (norm_offset + norm_code)`; equal scores order by ascending
//! document ID.
//!
//! Floats are model and ingest inputs only. A [`PackedCode`] contains no float,
//! and [`PreparedQuery`] scores one without reconstructing a vector. The model
//! validates a conservative all-query score bound at construction and decode,
//! before any row can be served. Document encoding uses four independent
//! float accumulators for 512-dimensional signs only when a conservative
//! rounding bound proves they have the same sign as the original serial dot;
//! uncertain decisions retain that serial operation in its original order.
//! The batch encoder divides one caller-bounded slice among scoped workers,
//! collects by input position, and leaves all index mutations to one writer.

use std::fmt;

use haiiie_core::{DocId, Index, IndexMeta, RowScorer, RowSearch, SetStore};
use sha2::{Digest, Sha256};

/// Residual signs stored in every document code.
///
/// The seven-way 64-byte allocation study selected 500 signs plus 12 norm bits
/// on validation; three fresh encoder fits and a new query split confirmed it.
pub const RESIDUAL_SIGNS: usize = 500;
/// Bits reserved for the document's reciprocal-norm code.
///
/// This is the remainder of the validation-selected 512-bit allocation above.
pub const NORM_BITS: u32 = 12;
/// Bytes in one complete stored document code.
pub const PACKED_CODE_BYTES: usize = 64;
/// Largest representable reciprocal-norm code.
pub const MAX_NORM_CODE: u16 = (1 << NORM_BITS) - 1;

const FULL_SIGN_BYTES: usize = 62;
const TAIL_SIGNS: usize = 4;
const MODEL_MAGIC: [u8; 8] = *b"haiir500";
const MODEL_VERSION: u16 = 1;
const MODEL_HEADER_BYTES: usize = 68;
// The fixed-point format uses the full positive signed-16-bit query range.
const QUERY_SCALE: f64 = 32_767.0;
// The fitted dictionary is symmetrically scaled to signed 12-bit magnitude.
const MAX_DECODER_COEFFICIENT: i16 = 2_047;

/// Stable identity of an offline-fitted model.
///
/// The identity is SHA-256 over the canonical model contents and is persisted
/// beside every coefficient. An index integration stores the same value in its metadata and
/// compares it before preparing a query; the 64-byte document rows deliberately
/// do not repeat 32 identity bytes apiece.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ModelId([u8; 32]);

impl ModelId {
    /// Reconstruct an identity stored in index metadata.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// The persisted bytes.
    #[must_use]
    pub const fn as_bytes(self) -> [u8; 32] {
        self.0
    }
}

impl fmt::Display for ModelId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for ModelId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

/// A residual model or input that cannot produce the specified exact codec.
#[derive(Debug)]
#[non_exhaustive]
pub enum CodecError {
    /// A vector or coefficient array has the wrong width.
    DimensionMismatch {
        /// Required number of elements.
        want: usize,
        /// Supplied number of elements.
        got: usize,
    },
    /// One fixed-width field does not fit its assigned bits.
    FieldOutOfRange(&'static str),
    /// Model coefficients or quantizer parameters violate the codec contract.
    InvalidModel(&'static str),
    /// An input vector cannot be normalized and encoded.
    InvalidVector(&'static str),
    /// An index contains fixed-format rows but names no external model.
    IndexHasNoModelIdentity,
    /// A loaded model is not the model named by an index.
    ModelMismatch {
        /// Identity required by the index.
        expected: ModelId,
        /// Identity carried by the loaded model.
        found: ModelId,
    },
    /// The complete conservative score bound does not fit signed 64-bit math.
    ScoreOverflow,
    /// Persisted model bytes are not one complete supported model.
    CorruptModel(&'static str),
}

impl fmt::Display for CodecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DimensionMismatch { want, got } => {
                write!(f, "expected {want} elements, got {got}")
            }
            Self::FieldOutOfRange(field) => write!(f, "{field} does not fit its stored field"),
            Self::InvalidModel(reason) => write!(f, "invalid residual model: {reason}"),
            Self::InvalidVector(reason) => write!(f, "invalid input vector: {reason}"),
            Self::IndexHasNoModelIdentity => write!(f, "index has no codec model identity"),
            Self::ModelMismatch { expected, found } => {
                write!(
                    f,
                    "index requires residual model {expected}, loaded {found}"
                )
            }
            Self::ScoreOverflow => write!(f, "residual model's score bound does not fit i64"),
            Self::CorruptModel(reason) => write!(f, "corrupt residual model: {reason}"),
        }
    }
}

impl std::error::Error for CodecError {}

/// One complete 64-byte document code.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct PackedCode([u8; PACKED_CODE_BYTES]);

impl fmt::Debug for PackedCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PackedCode")
            .field("norm_code", &self.norm_code())
            .finish_non_exhaustive()
    }
}

impl PackedCode {
    /// Pack exactly 500 signs and one 12-bit reciprocal-norm code.
    pub fn new(signs: &[bool], norm_code: u16) -> Result<Self, CodecError> {
        if signs.len() != RESIDUAL_SIGNS {
            return Err(CodecError::DimensionMismatch {
                want: RESIDUAL_SIGNS,
                got: signs.len(),
            });
        }
        if norm_code > MAX_NORM_CODE {
            return Err(CodecError::FieldOutOfRange("norm code"));
        }
        let mut bytes = [0u8; PACKED_CODE_BYTES];
        for (bit, &positive) in signs.iter().enumerate() {
            if positive {
                bytes[bit / 8] |= 1 << (bit % 8);
            }
        }
        let tail = (norm_code << TAIL_SIGNS) | u16::from(bytes[FULL_SIGN_BYTES] & 0x0f);
        bytes[FULL_SIGN_BYTES..].copy_from_slice(&tail.to_le_bytes());
        Ok(Self(bytes))
    }

    /// Adopt one already packed row. Every 512-bit pattern is a valid row.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; PACKED_CODE_BYTES]) -> Self {
        Self(bytes)
    }

    /// The exact persisted row.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; PACKED_CODE_BYTES] {
        &self.0
    }

    /// Whether residual sign `index` is positive.
    #[must_use]
    pub fn sign(&self, index: usize) -> Option<bool> {
        (index < RESIDUAL_SIGNS).then(|| self.0[index / 8] >> (index % 8) & 1 != 0)
    }

    /// The unsigned 12-bit reciprocal-norm code.
    #[must_use]
    pub fn norm_code(&self) -> u16 {
        u16::from_le_bytes([self.0[62], self.0[63]]) >> TAIL_SIGNS
    }

    /// Convert the byte layout to the eight little-endian words accepted by a
    /// 512-bit core index.
    #[must_use]
    pub fn to_words(self) -> [u64; 8] {
        std::array::from_fn(|word| {
            u64::from_le_bytes(
                self.0[word * 8..(word + 1) * 8]
                    .try_into()
                    .expect("one complete eight-byte word"),
            )
        })
    }

    /// Reconstruct a packed code from the words read from a 512-bit core index.
    pub fn from_words(words: &[u64]) -> Result<Self, CodecError> {
        if words.len() != 8 {
            return Err(CodecError::DimensionMismatch {
                want: 8,
                got: words.len(),
            });
        }
        let mut bytes = [0u8; PACKED_CODE_BYTES];
        for (word, value) in words.iter().enumerate() {
            bytes[word * 8..(word + 1) * 8].copy_from_slice(&value.to_le_bytes());
        }
        Ok(Self(bytes))
    }
}

/// The complete shared model used for document and query encoding.
#[derive(Clone, Debug)]
pub struct ResidualModel {
    id: ModelId,
    dims: u32,
    norm_offset: i64,
    norm_step: f64,
    /// Row-major `RESIDUAL_SIGNS * dims`, used only to encode documents.
    encoder: Vec<f32>,
    /// Per-atom L1 bounds for the exact-sign fast path at 512 dimensions.
    encoder_l1: Vec<f64>,
    /// Row-major `RESIDUAL_SIGNS * dims`, used for norms and query weights.
    decoder: Vec<i16>,
    max_abs_score: i64,
    max_decoded_squared_norm: i64,
}

impl ResidualModel {
    /// Validate and construct one fitted model.
    ///
    /// Decoder coefficients must be in the study's symmetric signed-12-bit
    /// range `[-2047, 2047]`. `norm_step` is retained for document ingest only;
    /// scoring uses the integer `norm_offset` and stored norm code.
    pub fn new(
        dims: u32,
        norm_offset: i64,
        norm_step: f64,
        encoder: Vec<f32>,
        decoder: Vec<i16>,
    ) -> Result<Self, CodecError> {
        if dims == 0 {
            return Err(CodecError::InvalidModel("input width is zero"));
        }
        let coefficients =
            RESIDUAL_SIGNS
                .checked_mul(dims as usize)
                .ok_or(CodecError::InvalidModel(
                    "coefficient count overflows usize",
                ))?;
        if encoder.len() != coefficients {
            return Err(CodecError::DimensionMismatch {
                want: coefficients,
                got: encoder.len(),
            });
        }
        if decoder.len() != coefficients {
            return Err(CodecError::DimensionMismatch {
                want: coefficients,
                got: decoder.len(),
            });
        }
        if !norm_step.is_finite() || norm_step <= 0.0 {
            return Err(CodecError::InvalidModel(
                "reciprocal-norm step is not finite and positive",
            ));
        }
        if norm_offset <= 0 || norm_offset > i64::MAX - i64::from(MAX_NORM_CODE) {
            return Err(CodecError::InvalidModel(
                "reciprocal-norm offset cannot produce positive i64 multipliers",
            ));
        }
        if encoder.iter().any(|value| !value.is_finite()) {
            return Err(CodecError::InvalidModel(
                "document encoder contains a non-finite coefficient",
            ));
        }
        if encoder
            .chunks_exact(dims as usize)
            .any(|atom| atom.iter().all(|&value| value == 0.0))
        {
            return Err(CodecError::InvalidModel(
                "document encoder contains an all-zero atom",
            ));
        }
        if decoder
            .iter()
            .any(|&value| !(-MAX_DECODER_COEFFICIENT..=MAX_DECODER_COEFFICIENT).contains(&value))
        {
            return Err(CodecError::InvalidModel(
                "decoder coefficient exceeds the symmetric signed-12-bit range",
            ));
        }

        // For any normalized input, each quantized query component has absolute
        // value at most 32767. Triangle inequality then bounds every possible
        // signed code dot without depending on a measured query corpus.
        let decoder_l1: i128 = decoder.iter().map(|&value| i128::from(value).abs()).sum();
        let dot_bound = i128::from(32_767) * decoder_l1;
        let multiplier_bound = i128::from(norm_offset + i64::from(MAX_NORM_CODE));
        let score_bound = dot_bound
            .checked_mul(multiplier_bound)
            .ok_or(CodecError::ScoreOverflow)?;
        let max_abs_score = i64::try_from(score_bound).map_err(|_| CodecError::ScoreOverflow)?;

        // Document norm accumulation is exact i64 math. Bound it for every sign
        // assignment, rather than trusting norms observed on a training corpus.
        let mut squared_norm_bound = 0i128;
        for dimension in 0..dims as usize {
            let component_bound: i128 = (0..RESIDUAL_SIGNS)
                .map(|sign| i128::from(decoder[sign * dims as usize + dimension]).abs())
                .sum();
            squared_norm_bound = squared_norm_bound
                .checked_add(component_bound * component_bound)
                .ok_or(CodecError::ScoreOverflow)?;
        }
        let max_decoded_squared_norm =
            i64::try_from(squared_norm_bound).map_err(|_| CodecError::ScoreOverflow)?;

        let encoder_l1 = if dims == 512 {
            encoder
                .chunks_exact(dims as usize)
                .map(|atom| atom.iter().map(|&value| f64::from(value).abs()).sum())
                .collect()
        } else {
            Vec::new()
        };
        let id = fingerprint(dims, norm_offset, norm_step, &encoder, &decoder);
        Ok(Self {
            id,
            dims,
            norm_offset,
            norm_step,
            encoder,
            encoder_l1,
            decoder,
            max_abs_score,
            max_decoded_squared_norm,
        })
    }

    /// Stable identity stored in the model header.
    #[must_use]
    pub const fn id(&self) -> ModelId {
        self.id
    }

    /// Input embedding width.
    #[must_use]
    pub const fn dims(&self) -> u32 {
        self.dims
    }

    /// Integer base added to every stored norm code.
    #[must_use]
    pub const fn norm_offset(&self) -> i64 {
        self.norm_offset
    }

    /// Conservative maximum absolute score over every normalized query and code.
    #[must_use]
    pub const fn max_abs_score(&self) -> i64 {
        self.max_abs_score
    }

    /// Conservative maximum decoded squared norm over every sign assignment.
    #[must_use]
    pub const fn max_decoded_squared_norm(&self) -> i64 {
        self.max_decoded_squared_norm
    }

    /// Refuse an index/model mismatch before any document rows are scored.
    pub fn require_index_meta(&self, meta: IndexMeta) -> Result<(), CodecError> {
        require_index_meta(self.id, meta)
    }

    /// Refuse a model-identity mismatch from another persistence boundary.
    pub fn require_id(&self, expected: ModelId) -> Result<(), CodecError> {
        if self.id == expected {
            Ok(())
        } else {
            Err(CodecError::ModelMismatch {
                expected,
                found: self.id,
            })
        }
    }

    /// Encode a float document into the complete fixed-size stored row.
    pub fn encode_document(&self, vector: &[f32]) -> Result<PackedCode, CodecError> {
        let mut residual = normalize(vector, self.dims as usize)?;
        let width = self.dims as usize;
        let mut signs = [false; RESIDUAL_SIGNS];
        let fast_signs = width == 512;
        let mut max_residual = if fast_signs {
            residual.iter().map(|value| value.abs()).fold(0.0, f32::max)
        } else {
            0.0
        };
        for (sign, atom) in self.encoder.chunks_exact(width).enumerate() {
            let positive = if fast_signs {
                bounded_four_way_sign(&residual, atom, max_residual, self.encoder_l1[sign])
            } else {
                serial_sign(&residual, atom)
            };
            signs[sign] = positive;
            let direction = if positive { 1.0 } else { -1.0 };
            if fast_signs {
                max_residual = 0.0;
                for (value, &coefficient) in residual.iter_mut().zip(atom) {
                    *value -= direction * coefficient;
                    max_residual = max_residual.max(value.abs());
                }
            } else {
                for (value, &coefficient) in residual.iter_mut().zip(atom) {
                    *value -= direction * coefficient;
                }
            }
        }

        // Decoder rows are contiguous by sign. Walking dimensions first took
        // one coefficient from each row with a `width * 2` byte stride. Visit
        // each row once and accumulate its contribution to every dimension.
        // The validated coefficient range bounds each partial component by
        // 500 * 2047 = 1_023_500, so i32 is exact even at the largest model.
        let mut decoded = vec![0i32; width];
        for (&positive, row) in signs.iter().zip(self.decoder.chunks_exact(width)) {
            for (component, &coefficient) in decoded.iter_mut().zip(row) {
                *component += if positive {
                    i32::from(coefficient)
                } else {
                    -i32::from(coefficient)
                };
            }
        }

        let mut squared_norm = 0i64;
        for component in decoded {
            let decoded = i64::from(component);
            squared_norm = squared_norm
                .checked_add(
                    decoded
                        .checked_mul(decoded)
                        .ok_or(CodecError::ScoreOverflow)?,
                )
                .ok_or(CodecError::ScoreOverflow)?;
        }
        if squared_norm == 0 {
            return Err(CodecError::InvalidModel(
                "document decodes to a zero-length vector",
            ));
        }
        debug_assert!(squared_norm <= self.max_decoded_squared_norm);
        let reciprocal_norm = 1.0 / (squared_norm as f64).sqrt();
        let raw = (reciprocal_norm / self.norm_step).round_ties_even() - self.norm_offset as f64;
        let norm_code = raw.clamp(0.0, f64::from(MAX_NORM_CODE)) as u16;
        PackedCode::new(&signs, norm_code)
    }

    /// Encode one caller-bounded batch in input order, using up to `workers`
    /// scoped threads. Zero workers means one. The caller controls the batch
    /// size and applies the returned codes through one ordered writer; an
    /// encoding error returns no codes, so no partial batch reaches the writer.
    /// The first error is the first invalid input, independent of scheduling.
    pub fn encode_documents(
        &self,
        vectors: &[&[f32]],
        workers: usize,
    ) -> Result<Vec<PackedCode>, CodecError> {
        if workers <= 1 || vectors.len() <= 1 {
            return vectors
                .iter()
                .map(|vector| self.encode_document(vector))
                .collect();
        }
        let width = vectors.len().div_ceil(workers.min(vectors.len()));
        let parts = std::thread::scope(|scope| {
            let handles: Vec<_> = vectors
                .chunks(width)
                .map(|chunk| {
                    scope.spawn(move || {
                        chunk
                            .iter()
                            .map(|vector| self.encode_document(vector))
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|handle| handle.join().expect("document encoder worker panicked"))
                .collect::<Vec<_>>()
        });
        parts.into_iter().flatten().collect()
    }

    /// Normalize and prepare one query for repeated exact packed-row scoring.
    pub fn prepare_query(&self, vector: &[f32]) -> Result<PreparedQuery, CodecError> {
        let normalized = normalize(vector, self.dims as usize)?;
        let quantized: Vec<i64> = normalized
            .into_iter()
            .map(|value| (f64::from(value) * QUERY_SCALE).round_ties_even() as i64)
            .collect();
        let width = self.dims as usize;
        let mut weights = vec![0i64; RESIDUAL_SIGNS];
        for (sign, row) in self.decoder.chunks_exact(width).enumerate() {
            weights[sign] = quantized
                .iter()
                .zip(row)
                .map(|(&query, &coefficient)| query * i64::from(coefficient))
                .sum();
        }
        PreparedQuery::new(self.id, self.norm_offset, self.max_abs_score, weights)
    }

    /// Serialize the full ingest/scoring model in a fixed little-endian format.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(
            MODEL_HEADER_BYTES + self.encoder.len() * 4 + self.decoder.len() * 2,
        );
        bytes.extend_from_slice(&MODEL_MAGIC);
        bytes.extend_from_slice(&MODEL_VERSION.to_le_bytes());
        bytes.extend_from_slice(&(RESIDUAL_SIGNS as u16).to_le_bytes());
        bytes.extend_from_slice(&(NORM_BITS as u16).to_le_bytes());
        bytes.extend_from_slice(&0u16.to_le_bytes());
        bytes.extend_from_slice(&self.dims.to_le_bytes());
        bytes.extend_from_slice(&self.id.0);
        bytes.extend_from_slice(&self.norm_offset.to_le_bytes());
        bytes.extend_from_slice(&self.norm_step.to_le_bytes());
        debug_assert_eq!(bytes.len(), MODEL_HEADER_BYTES);
        for &value in &self.encoder {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        for &value in &self.decoder {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        bytes
    }

    /// Parse and revalidate bytes produced by [`ResidualModel::to_bytes`].
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, CodecError> {
        if bytes.len() < MODEL_HEADER_BYTES {
            return Err(CodecError::CorruptModel("model is shorter than its header"));
        }
        if bytes[..8] != MODEL_MAGIC {
            return Err(CodecError::CorruptModel("model magic does not match"));
        }
        let u16_at = |at: usize| u16::from_le_bytes([bytes[at], bytes[at + 1]]);
        let u32_at = |at: usize| {
            u32::from_le_bytes(bytes[at..at + 4].try_into().expect("four checked bytes"))
        };
        let version = u16_at(8);
        if version != MODEL_VERSION {
            return Err(CodecError::CorruptModel("model version is unsupported"));
        }
        if usize::from(u16_at(10)) != RESIDUAL_SIGNS
            || u32::from(u16_at(12)) != NORM_BITS
            || u16_at(14) != 0
        {
            return Err(CodecError::CorruptModel(
                "fixed codec geometry does not match",
            ));
        }
        let dims = u32_at(16);
        let coefficients =
            RESIDUAL_SIGNS
                .checked_mul(dims as usize)
                .ok_or(CodecError::CorruptModel(
                    "coefficient count overflows usize",
                ))?;
        let expected = MODEL_HEADER_BYTES
            .checked_add(coefficients.checked_mul(6).ok_or(CodecError::CorruptModel(
                "model byte length overflows usize",
            ))?)
            .ok_or(CodecError::CorruptModel(
                "model byte length overflows usize",
            ))?;
        if bytes.len() != expected {
            return Err(CodecError::CorruptModel("model byte length does not match"));
        }
        let stored_id = ModelId::from_bytes(bytes[20..52].try_into().expect("32 checked bytes"));
        let norm_offset = i64::from_le_bytes(bytes[52..60].try_into().expect("8 checked bytes"));
        let norm_step = f64::from_le_bytes(bytes[60..68].try_into().expect("8 checked bytes"));
        let encoder_end = MODEL_HEADER_BYTES + coefficients * 4;
        let encoder = bytes[MODEL_HEADER_BYTES..encoder_end]
            .chunks_exact(4)
            .map(|chunk| f32::from_le_bytes(chunk.try_into().expect("four bytes")))
            .collect();
        let decoder = bytes[encoder_end..]
            .chunks_exact(2)
            .map(|chunk| i16::from_le_bytes(chunk.try_into().expect("two bytes")))
            .collect();
        let model = Self::new(dims, norm_offset, norm_step, encoder, decoder)?;
        if model.id != stored_id {
            return Err(CodecError::CorruptModel(
                "model identity does not match its contents",
            ));
        }
        Ok(model)
    }
}

fn fingerprint(
    dims: u32,
    norm_offset: i64,
    norm_step: f64,
    encoder: &[f32],
    decoder: &[i16],
) -> ModelId {
    let mut hash = Sha256::new();
    hash.update(MODEL_MAGIC);
    hash.update(MODEL_VERSION.to_le_bytes());
    hash.update((RESIDUAL_SIGNS as u16).to_le_bytes());
    hash.update((NORM_BITS as u16).to_le_bytes());
    hash.update(0u16.to_le_bytes());
    hash.update(dims.to_le_bytes());
    hash.update(norm_offset.to_le_bytes());
    hash.update(norm_step.to_le_bytes());
    for &value in encoder {
        hash.update(value.to_le_bytes());
    }
    for &value in decoder {
        hash.update(value.to_le_bytes());
    }
    ModelId(hash.finalize().into())
}

/// One exact score result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResidualHit {
    /// Document ordinal.
    pub id: DocId,
    /// Exact integer score under the stored-code metric.
    pub score: i64,
}

/// Query-specific lookup tables for the fixed 64-byte scorer.
#[derive(Clone, Debug)]
pub struct PreparedQuery {
    model_id: ModelId,
    norm_offset: i64,
    max_abs_score: i64,
    weights: Vec<i64>,
    full: Vec<i64>,
    tail: [i64; 1 << TAIL_SIGNS],
}

impl PreparedQuery {
    fn new(
        model_id: ModelId,
        norm_offset: i64,
        max_abs_score: i64,
        weights: Vec<i64>,
    ) -> Result<Self, CodecError> {
        let observed_dot_bound: i128 = weights.iter().map(|&weight| i128::from(weight).abs()).sum();
        let observed_score_bound =
            observed_dot_bound * i128::from(norm_offset + i64::from(MAX_NORM_CODE));
        if observed_score_bound > i128::from(max_abs_score) {
            return Err(CodecError::ScoreOverflow);
        }
        let mut full = vec![0i64; FULL_SIGN_BYTES * 256];
        for byte in 0..FULL_SIGN_BYTES {
            for value in 0..256usize {
                full[byte * 256 + value] = (0..8)
                    .map(|bit| {
                        let weight = weights[byte * 8 + bit];
                        if value & (1 << bit) == 0 {
                            -weight
                        } else {
                            weight
                        }
                    })
                    .sum();
            }
        }
        let mut tail = [0i64; 1 << TAIL_SIGNS];
        for (value, slot) in tail.iter_mut().enumerate() {
            *slot = (0..TAIL_SIGNS)
                .map(|bit| {
                    let weight = weights[496 + bit];
                    if value & (1 << bit) == 0 {
                        -weight
                    } else {
                        weight
                    }
                })
                .sum();
        }
        Ok(Self {
            model_id,
            norm_offset,
            max_abs_score,
            weights,
            full,
            tail,
        })
    }

    /// Model identity this query was prepared against.
    #[must_use]
    pub const fn model_id(&self) -> ModelId {
        self.model_id
    }

    /// Refuse an index that cannot contain rows for this prepared query.
    pub fn require_index_meta(&self, meta: IndexMeta) -> Result<(), CodecError> {
        require_index_meta(self.model_id, meta)
    }

    /// Begin exact filter-aware scoring over an index's persisted rows.
    ///
    /// The model identity and fixed 512-bit row width are checked before the
    /// core opens a snapshot. The returned search borrows rows from the store,
    /// applies this query's integer lookup tables and keeps only block-local
    /// top-k state across the scan.
    pub fn search<'i, 'q, S: SetStore>(
        &'q self,
        index: &'i Index<S>,
    ) -> Result<RowSearch<'i, 'q, S, Self>, CodecError> {
        self.require_index_meta(index.meta())?;
        Ok(index.row_search(self))
    }

    /// The 500 exact integer sign weights, useful for a slow independent oracle.
    #[must_use]
    pub fn weights(&self) -> &[i64] {
        &self.weights
    }

    /// Exact score of one stored row.
    #[must_use]
    pub fn score(&self, code: &PackedCode) -> i64 {
        let row = code.as_bytes();
        let mut dot = 0i64;
        for (byte, &value) in row[..FULL_SIGN_BYTES].iter().enumerate() {
            dot += self.full[byte * 256 + usize::from(value)];
        }
        let tail = u16::from_le_bytes([row[62], row[63]]);
        dot += self.tail[usize::from(tail & 0x0f)];
        let score = dot
            .checked_mul(self.norm_offset + i64::from(tail >> TAIL_SIGNS))
            .expect("validated model and query bounds make every packed score fit i64");
        debug_assert!(score.unsigned_abs() <= self.max_abs_score as u64);
        score
    }

    fn score_words(&self, row: &[u64]) -> i64 {
        debug_assert_eq!(row.len(), PACKED_CODE_BYTES / 8);
        let mut dot = 0i64;
        for byte in 0..FULL_SIGN_BYTES {
            let value = ((row[byte / 8] >> ((byte % 8) * 8)) & 0xff) as usize;
            dot += self.full[byte * 256 + value];
        }
        let tail = (row[7] >> 48) as u16;
        dot += self.tail[usize::from(tail & 0x0f)];
        let score = dot
            .checked_mul(self.norm_offset + i64::from(tail >> TAIL_SIGNS))
            .expect("validated model and query bounds make every packed score fit i64");
        debug_assert!(score.unsigned_abs() <= self.max_abs_score as u64);
        score
    }

    /// Return the true top-k of the supplied stored codes.
    ///
    /// Results order by descending exact score and then ascending document ID.
    #[must_use]
    pub fn top_k<'a>(
        &self,
        documents: impl IntoIterator<Item = (DocId, &'a PackedCode)>,
        k: usize,
    ) -> Vec<ResidualHit> {
        let mut top = Vec::with_capacity(k);
        for (id, code) in documents {
            let hit = ResidualHit {
                id,
                score: self.score(code),
            };
            let at = top.partition_point(|old: &ResidualHit| {
                old.score > hit.score || (old.score == hit.score && old.id < hit.id)
            });
            if at < k {
                top.insert(at, hit);
                if top.len() > k {
                    top.pop();
                }
            }
        }
        top
    }
}

impl RowScorer for PreparedQuery {
    fn score(&self, row: &[u64]) -> i64 {
        self.score_words(row)
    }
}

fn require_index_meta(model_id: ModelId, meta: IndexMeta) -> Result<(), CodecError> {
    if meta.dims != (PACKED_CODE_BYTES * 8) as u32 {
        return Err(CodecError::DimensionMismatch {
            want: PACKED_CODE_BYTES * 8,
            got: meta.dims as usize,
        });
    }
    let stored = meta.model_id.ok_or(CodecError::IndexHasNoModelIdentity)?;
    let expected = ModelId::from_bytes(stored);
    if model_id == expected {
        Ok(())
    } else {
        Err(CodecError::ModelMismatch {
            expected,
            found: model_id,
        })
    }
}

fn serial_sign(residual: &[f32], atom: &[f32]) -> bool {
    let dot: f32 = residual
        .iter()
        .zip(atom)
        .map(|(left, right)| left * right)
        .sum();
    dot >= 0.0
}

fn bounded_four_way_sign(residual: &[f32], atom: &[f32], max_residual: f32, atom_l1: f64) -> bool {
    debug_assert_eq!(residual.len(), 512);
    let mut sums = [0.0f32; 4];
    for (values, coefficients) in residual.chunks_exact(4).zip(atom.chunks_exact(4)) {
        for lane in 0..4 {
            sums[lane] += values[lane] * coefficients[lane];
        }
    }
    let approximate: f32 = sums.into_iter().sum();
    let absolute_sum_bound = f64::from(max_residual) * atom_l1;
    // For 512 products, f32 unit roundoff u=2^-24 gives gamma_512 < 3.06e-5
    // for the serial sum. Four 128-term sums plus three final additions add
    // less than 7.82e-6. Both sum the same rounded products, whose absolute
    // sum is bounded by max_residual * atom_l1 up to product rounding. 1e-4
    // exceeds the combined <3.85e-5 bound with margin; 1e-30 also covers all
    // subnormal rounding across 512 terms. If that margin does not establish
    // the sign, evaluate the original scalar expression in its original order.
    // The half-MAX check rules out intermediate f32 overflow in either sum.
    let error_bound = 0.0001 * absolute_sum_bound + 1e-30;
    if approximate.is_finite()
        && absolute_sum_bound < f64::from(f32::MAX) * 0.5
        && f64::from(approximate).abs() > error_bound
    {
        approximate >= 0.0
    } else {
        serial_sign(residual, atom)
    }
}

fn normalize(vector: &[f32], dims: usize) -> Result<Vec<f32>, CodecError> {
    if vector.len() != dims {
        return Err(CodecError::DimensionMismatch {
            want: dims,
            got: vector.len(),
        });
    }
    if vector.iter().any(|value| !value.is_finite()) {
        return Err(CodecError::InvalidVector("input vector is not finite"));
    }
    let squared_norm: f64 = vector
        .iter()
        .map(|&value| f64::from(value) * f64::from(value))
        .sum();
    if squared_norm == 0.0 || !squared_norm.is_finite() {
        return Err(CodecError::InvalidVector(
            "input vector does not have a finite positive norm",
        ));
    }
    let norm = squared_norm.sqrt();
    Ok(vector
        .iter()
        .map(|&value| (f64::from(value) / norm) as f32)
        .collect())
}

#[cfg(test)]
mod sign_tests {
    use super::{bounded_four_way_sign, serial_sign};
    use crate::rng::Rng;

    fn assert_same_sign(residual: &[f32; 512], atom: &[f32; 512]) {
        let max_residual = residual.iter().map(|v| v.abs()).fold(0.0, f32::max);
        let atom_l1: f64 = atom.iter().map(|&v| f64::from(v).abs()).sum();
        assert_eq!(
            bounded_four_way_sign(residual, atom, max_residual, atom_l1),
            serial_sign(residual, atom),
        );
    }

    fn serial_code(model: &super::ResidualModel, vector: &[f32]) -> super::PackedCode {
        let width = model.dims as usize;
        let mut residual = super::normalize(vector, width).expect("normalize");
        let mut signs = [false; super::RESIDUAL_SIGNS];
        for (sign, atom) in model.encoder.chunks_exact(width).enumerate() {
            let dot: f32 = residual
                .iter()
                .zip(atom)
                .map(|(left, right)| left * right)
                .sum();
            let positive = dot >= 0.0;
            signs[sign] = positive;
            let direction = if positive { 1.0 } else { -1.0 };
            for (value, &coefficient) in residual.iter_mut().zip(atom) {
                *value -= direction * coefficient;
            }
        }
        let mut decoded = vec![0i32; width];
        for (&positive, row) in signs.iter().zip(model.decoder.chunks_exact(width)) {
            for (component, &coefficient) in decoded.iter_mut().zip(row) {
                *component += if positive {
                    i32::from(coefficient)
                } else {
                    -i32::from(coefficient)
                };
            }
        }
        let squared_norm: i64 = decoded.iter().map(|&value| i64::from(value).pow(2)).sum();
        assert!(squared_norm > 0);
        let reciprocal_norm = 1.0 / (squared_norm as f64).sqrt();
        let raw = (reciprocal_norm / model.norm_step).round_ties_even() - model.norm_offset as f64;
        let norm_code = raw.clamp(0.0, f64::from(super::MAX_NORM_CODE)) as u16;
        super::PackedCode::new(&signs, norm_code).expect("pack")
    }

    #[test]
    fn fast_512_encoder_matches_the_retained_serial_code() {
        let width = 512usize;
        let mut encoder = vec![0.0f32; super::RESIDUAL_SIGNS * width];
        let mut decoder = vec![0i16; encoder.len()];
        for sign in 0..super::RESIDUAL_SIGNS {
            for dimension in 0..width {
                encoder[sign * width + dimension] =
                    (((sign * 31 + dimension * 17) % 257) as f32 - 128.0) * 0.00001;
            }
            decoder[sign * width] = if sign == 0 { 1 } else { 2 };
        }
        let model =
            super::ResidualModel::new(512, 5000, 1e-7, encoder, decoder).expect("bounded model");
        let reopened = super::ResidualModel::from_bytes(&model.to_bytes()).expect("reopen model");
        let mut rng = Rng::new(0x0068_6169_6965);
        for document in 0..128usize {
            let mut values: Vec<f32> = (0..width).map(|_| rng.next_f32() * 2.0 - 1.0).collect();
            if document == 0 {
                values.fill(1.0);
            } else if document == 1 {
                for (dimension, value) in values.iter_mut().enumerate() {
                    *value = if dimension % 2 == 0 { 1.0 } else { -1.0 };
                }
            }
            let want = serial_code(&model, &values);
            assert_eq!(model.encode_document(&values).expect("fast encode"), want);
            assert_eq!(
                reopened.encode_document(&values).expect("reopened fast"),
                want
            );
        }
    }

    #[test]
    fn bounded_sign_matches_serial_at_cancellation_underflow_and_overflow() {
        let residual = [1.0f32; 512];
        let mut atom = [0.0f32; 512];
        for value in &mut atom[..256] {
            *value = 0.125;
        }
        for value in &mut atom[256..] {
            *value = -0.125;
        }
        assert_same_sign(&residual, &atom);
        atom[511] = -0.125 + f32::EPSILON;
        assert_same_sign(&residual, &atom);
        atom.fill(f32::MIN_POSITIVE / 100.0);
        assert_same_sign(&residual, &atom);
        atom[..256].fill(f32::MAX / 4.0);
        atom[256..].fill(-f32::MAX / 4.0);
        assert_same_sign(&residual, &atom);

        let mut rng = Rng::new(0x1234_5678);
        for _ in 0..1024 {
            let residual = std::array::from_fn(|_| rng.next_f32() * 2.0 - 1.0);
            let atom = std::array::from_fn(|_| rng.next_f32() * 2.0 - 1.0);
            assert_same_sign(&residual, &atom);
        }
    }
}
