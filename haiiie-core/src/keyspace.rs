//! The yesnodb key namespace haiiie occupies.
//!
//! # One struct owns every key, and nothing else constructs one
//!
//! Keys are numbers on disk. If two call sites compute one the same way by
//! coincidence, the format is whatever they happen to agree on, and a change to
//! one is a silent corruption of the other. So every key in this crate comes
//! from here. `.agents/docs/ARCHITECTURE.md` records which index kind writes
//! each view.
//!
//! # Layout
//!
//! ```text
//! key = (namespace << 56) | (kind << 20) | index
//! ```
//!
//! The namespace makes an index relocatable: several indexes or an index and
//! application data can share a database if their keys do not overlap. Creation
//! checks all defined kind ranges; later foreign writes must avoid them too.
//! yesnodb hashes keys to shards with SplitMix64, but a single large FWD key
//! remains on one shard, so key hashing does not balance bytes automatically.
//!
//! There is deliberately **no segment field**. An early design carried one to
//! bound `Snapshot::load`, on the premise that loading a key materialized its
//! bytes. That was false -- `load` aliases the mapping, so its cost is
//! allocations rather than resident bytes -- and upstream has since added
//! `Snapshot::key_stream` in any case.

/// What a key holds.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u64)]
#[non_exhaustive]
pub enum Kind {
    /// A bit-packed metadata blob: dimension count, geometry, optional codec-model identity.
    Meta = 0x01,
    /// The live document set. Deletion removes from this.
    Live = 0x02,
    /// Binary-index posting list: documents whose bit `d` is set.
    Dim = 0x10,
    /// Binary-index bit-plane `j` of `z = D - |x|`, the complement weight.
    ///
    /// The complement rather than the weight, so Hamming's query-time step is an
    /// addition ( `S = 2a + z` ) rather than a subtraction.
    ZPlane = 0x11,
    /// Doc-major forward codes under one key, addressed by 65536-ordinal chunks.
    Forward = 0x20,
    /// Binary-index per-block code-weight range.
    Stat = 0x21,
    /// A caller's own boolean attribute, usable directly in a filter.
    Attr = 0x30,
    /// Pending offline compaction: the source version, awaiting acknowledgement.
    Compaction = 0x40,
    /// Old IDs indexed by new ID, persisted with the compacted data.
    Remap = 0x41,
}

/// Constructs every key one haiiie index uses.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct KeySpace {
    namespace: u8,
}

impl KeySpace {
    /// The largest `index` any kind can address.
    pub const INDEX_MAX: u64 = (1 << 20) - 1;

    /// Reject an attribute term before it can carry into the kind field.
    pub(crate) fn check_term(term: u32) -> crate::Result<()> {
        let max = u32::try_from(Self::INDEX_MAX).unwrap_or(u32::MAX);
        if term > max {
            return Err(crate::Error::AttrTermTooLarge { term, max });
        }
        Ok(())
    }

    /// A key space in the given namespace.
    #[must_use]
    pub const fn new(namespace: u8) -> Self {
        Self { namespace }
    }

    /// This key space's namespace.
    #[must_use]
    pub const fn namespace(self) -> u8 {
        self.namespace
    }

    #[inline]
    #[must_use]
    const fn key(self, kind: Kind, index: u64) -> u64 {
        // Creation bounds dimensions and derived planes; max_doc_id bounds
        // statistics blocks, and write and query boundaries check attribute
        // terms. This assertion catches internal mistakes before an index
        // can carry into another kind's key.
        debug_assert!(
            index <= Self::INDEX_MAX,
            "key index overflows into the kind field"
        );
        ((self.namespace as u64) << 56) | ((kind as u64) << 20) | index
    }

    /// The metadata blob.
    #[inline]
    #[must_use]
    pub const fn meta(self) -> u64 {
        self.key(Kind::Meta, 0)
    }

    /// The live-document set.
    #[inline]
    #[must_use]
    pub const fn live(self) -> u64 {
        self.key(Kind::Live, 0)
    }

    /// The posting list for dimension `d`.
    #[inline]
    #[must_use]
    pub const fn dim(self, d: u32) -> u64 {
        self.key(Kind::Dim, d as u64)
    }

    /// Bit-plane `j` of the complement weight.
    #[inline]
    #[must_use]
    pub const fn zplane(self, j: u32) -> u64 {
        self.key(Kind::ZPlane, j as u64)
    }

    /// Every document's forward code.
    ///
    /// **One key, not one per block.** It was one per 65 536-ordinal chunk so
    /// that a whole-key `load` was bounded, back when the forward path read it
    /// that way; that path now reads a targeted chunk and the reason is gone.
    /// What the split cost was a fresh stream open per chunk -- the forward
    /// path's dominant cost at exactly the selectivities it is chosen for, and
    /// the same defect a held-open cursor fixed on the inverted side.
    ///
    /// No ordinal moves. A row still lives at `block * BLOCK_ORDINALS + row *
    /// row_bits`, which is already a global ordinal whose chunk index is
    /// `block`, so merging the keys leaves the contents of every chunk exactly
    /// where it was. It does retire the implicit ceiling the split carried: the
    /// block number occupied the 20-bit index field, capping an index at
    /// `2^20 * rows_per_block` documents.
    #[inline]
    #[must_use]
    pub const fn forward(self) -> u64 {
        self.key(Kind::Forward, 0)
    }

    /// Per-block statistics.
    #[inline]
    #[must_use]
    pub const fn stat(self, index: u64) -> u64 {
        self.key(Kind::Stat, index)
    }

    /// A caller attribute.
    #[inline]
    #[must_use]
    pub const fn attr(self, term: u32) -> u64 {
        self.key(Kind::Attr, term as u64)
    }

    /// The pending offline compaction marker.
    #[must_use]
    pub const fn compaction(self) -> u64 {
        self.key(Kind::Compaction, 0)
    }

    /// The durable ID mapping belonging to the pending compaction.
    #[must_use]
    pub const fn remap(self) -> u64 {
        self.key(Kind::Remap, 0)
    }

    /// Half-open key range for one kind, including its largest index.
    pub(crate) const fn kind_range(self, kind: Kind) -> (u64, u64) {
        (self.key(kind, 0), self.key(kind, Self::INDEX_MAX) + 1)
    }
}

/// Ordinals per block. One chunk of every posting list, by construction.
///
/// **Derived from the storage layer's own constant, not a literal that matches
/// it.** This said `1 << 16` beside a comment claiming the alignment held "by
/// construction", when what held it was two independent sixteens agreeing. The
/// alignment is not decoration: [`block_of`] produces a number that is handed
/// straight to a chunk `seek` as a **prefix**, so a block that is not exactly a
/// chunk does not merely lose an optimization -- it reads the wrong chunk.
///
/// A copy of a definition can fall behind its original silently, which is the
/// same defect that let an integrity assertion here check four of a report's
/// five findings while the storage layer's own predicate checked all five.
pub const BLOCK_ORDINALS: u64 = yesno_core::CHUNK_CARD as u64;

/// The block an ordinal belongs to, which is its chunk prefix.
#[inline]
#[must_use]
pub const fn block_of(ordinal: u64) -> u64 {
    ordinal >> yesno_core::CHUNK_BITS
}
