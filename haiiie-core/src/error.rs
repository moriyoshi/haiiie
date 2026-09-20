//! One error type for the whole crate.
//!
//! Every variant names a condition a caller can act on. `ReservedDocId` exists
//! because yesnodb reserves `u64::MAX` so that the cardinality of the complete
//! ordinal universe stays representable in a `u64` -- so the reservation is the
//! substrate's, and rejecting it at our boundary is how a caller learns that
//! before a write reaches the substrate and fails less legibly.

/// The crate's result type.
pub type Result<T> = std::result::Result<T, Error>;

/// Anything haiiie can refuse.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// External objects and ID-based filters must be remapped before serving.
    #[error(
        "namespace {0} has a pending compaction; recover its ID mapping with compact(), then acknowledge_compaction() before opening"
    )]
    CompactionPending(u8),

    /// An acknowledgement must name the pending compaction's source version.
    #[error("no pending compaction matches source version {0}")]
    CompactionMismatch(u64),

    /// `u64::MAX` is reserved by yesnodb and is not a usable document id.
    ///
    /// This is yesnodb's bound on the ordinal space. haiiie's forward layout
    /// imposes a **tighter, dimension-dependent** one -- see [`Error::DocIdTooLarge`]
    /// -- so passing this check does not mean an id is usable in an index.
    #[error("doc id {0} is reserved; yesnodb's ordinal space ends at u64::MAX - 1")]
    ReservedDocId(u64),

    /// An attribute term outside what the key space's index field can address.
    ///
    /// Same 20-bit field as [`Error::TooManyDimensions`] and a different remedy,
    /// which is why it is a different variant: dimensions are fixed when an
    /// index is created and cannot be changed afterwards, while terms are
    /// caller-assigned per document and can simply be renumbered.
    ///
    /// Hashing a string to a `u32` and using it as a term is the obvious way to
    /// reach this, and it was previously silent -- the term carried into the
    /// kind field and the attribute landed under a key belonging to no defined
    /// kind. No existing data collided, which is luck rather than design.
    #[error("attribute term {term} exceeds the {max} this key space can address")]
    AttrTermTooLarge { term: u32, max: u32 },

    /// More dimensions than this index's layout can hold.
    ///
    /// **Two constraints, and the tighter one has no arithmetic at its call
    /// site.** A key is `(namespace << 56) | (kind << 20) | index`, so a
    /// dimension of 2^20 or more carries into the kind field -- dimension
    /// 1 048 576 produced `(0x10 << 20) | (1 << 20)`, which is `0x11 << 20`,
    /// which is z-plane 0 exactly, so a posting list and a weight plane would
    /// have shared a key and returned a wrong score rather than an error.
    ///
    /// The binding one is the forward layout: a document's row is `row_bits`
    /// consecutive ordinals **inside one 65 536-ordinal block**, so
    /// `rows_per_block` is `65536 / row_bits` and a code wider than a block
    /// makes it **zero**. Every `put` then divided by it. The first version of
    /// this check bounded only the key field, at 2^20, and left every width
    /// between 65 537 and that creatable and unusable.
    #[error("{dims} dimensions exceed the {max} this key space can address")]
    TooManyDimensions { dims: u32, max: u32 },

    /// The id is inside yesnodb's ordinal space but outside what this index's
    /// forward layout can address.
    ///
    /// A document's forward row starts at `block * 65536 + row * row_bits`,
    /// which works out to `id * row_bits`, so the usable range shrinks as the
    /// code widens: about `u64::MAX / 256` at 256 dimensions. Nothing checked
    /// this. `DocId` is a tuple struct with a public field, so `DocId::new`'s
    /// guard is advisory and a caller reaching the limit got an arithmetic
    /// overflow -- a panic in debug and, in release, a wrapped address and a
    /// silent write into an unrelated document's row.
    #[error(
        "doc id {id} cannot be addressed by a {dims}-bit index; the usable range          here is [0, {max}]"
    )]
    DocIdTooLarge { id: u64, dims: u32, max: u64 },

    /// A snapshot was evicted while a scan was reading through it.
    ///
    /// Expressed in haiiie's own error type rather than only as the wrapped
    /// store error, because eviction is a property of the [`SetStore`] contract
    /// -- any store may bound how long a read view lives -- and a fault-injecting
    /// double must be able to raise it without constructing a yesnodb error.
    ///
    /// [`SetStore`]: crate::SetStore
    #[error("the snapshot at version {version} was evicted mid-scan")]
    SnapshotExpired {
        /// The version that is no longer readable.
        version: u64,
    },

    /// A file the index depends on could not be read or written.
    #[error(transparent)]
    Io(#[from] std::io::Error),

    /// The underlying set store refused.
    #[error(transparent)]
    Store(#[from] yesno_core::CodecError),

    /// An index already occupies this namespace.
    #[error("namespace {0} already holds an index; use `open` instead of `create`")]
    AlreadyExists(u8),

    /// Foreign data already occupies an index-owned key in this namespace.
    #[error("namespace {namespace} has data at index key {key}; choose an unused namespace")]
    KeyspaceOccupied { namespace: u8, key: u64 },

    /// The stored metadata is absent, truncated or self-inconsistent.
    #[error("{0}")]
    CorruptMeta(&'static str),

    /// The index was written by a different version of haiiie's on-disk layout.
    ///
    /// Distinct from [`Error::CorruptMeta`] on purpose. The header parsed, the
    /// magic matched, and nothing is damaged -- so telling the holder of a
    /// perfectly intact index that it is corrupt would send them looking for a
    /// fault that is not there. What they need to know is that the layout moved
    /// and the index has to be rebuilt.
    #[error(
        "index was written with layout version {found}; this build reads {expected}. \
         The layout changed and there is no in-place upgrade: rebuild the index."
    )]
    UnsupportedLayout {
        /// The version recorded in the index.
        found: u16,
        /// The version this build writes and reads.
        expected: u16,
    },

    /// Binary scoring cannot interpret a model-bound residual row.
    #[error("binary scoring is unavailable on a residual index; use the matching model")]
    BinarySearchOnResidualIndex,

    /// A code was presented at a width the index was not built for.
    #[error("code has {got} bits; this index has {want}")]
    DimensionMismatch {
        /// The index's configured width.
        want: u32,
        /// The width of the code that was offered.
        got: u32,
    },
}

impl Error {
    /// Whether this is an eviction, from whichever layer raised it.
    ///
    /// Both spellings mean the same thing to a scan -- the read view is gone,
    /// take a fresh one -- so the retry logic must not care which arrived.
    /// yesnodb's `SnapshotTooOld` carries a resume key that haiiie does not use:
    /// the scan resumes from the block it was on, which it already knows.
    #[must_use]
    pub fn is_snapshot_expired(&self) -> bool {
        matches!(
            self,
            Self::SnapshotExpired { .. }
                | Self::Store(yesno_core::CodecError::SnapshotTooOld { .. })
        )
    }
}
