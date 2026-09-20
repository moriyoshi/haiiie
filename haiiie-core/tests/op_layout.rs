use haiiie_core::Op;

/// `Op` must not grow past the widest variant that genuinely needs the space.
///
/// # This is a cost no test that counts operations can see
///
/// `Op` is the element type of every [`Batch`](haiiie_core::Batch) op vector,
/// and binary ingest pushes about **255 entries per document** into it. So the
/// width of this enum is multiplied by every set bit of every document, and
/// widening it is a throughput change disguised as a type change.
///
/// Measured, 2026-09-24, on this repository and upstream on the same day:
///
/// * Adding `PatchChunk(u64, u64, Container, Container)` took haiiie's `Op`
///   from **32 bytes to 128** -- `Container` is 56 -- a 4x widening. At
///   1 048 576 documents the op vector's byte traffic goes from 8.6 GB to
///   34.2 GB.
/// * Upstream shipped the same shape first and measured the consequence on its
///   own `Op` ( 72 to 128 bytes ): point ingest of 96 903 documents went from a
///   1.763 s mean to 2.471 s, and boxing the masks recovered **28.7%**.
/// * haiiie's own end-to-end measurement of upstream's copy of this defect:
///   binary ingest of 1 048 576 documents fell from 29 146 to 24 922-25 909
///   documents/second, with an encode-time control confirming the host.
///
/// **Two independent gates were green on this**, upstream's fifteen chunk-patch
/// tests with sabotage and live/replay checks, and haiiie's 174 including
/// allocation budgets. An allocation budget cannot catch it: the operation
/// *count* never changed, only the width of each one, and nothing else asserts
/// a type's size.
///
/// The bound is stated **relatively**, against the widest payload an `Op`
/// variant actually requires, so a legitimate change to `Container` or to
/// haiiie's own key and ordinal types does not turn this into a constant
/// someone raises to go green. If this fails, box the offending variant's
/// payload rather than relaxing the bound -- that is what the measurement above
/// is here to make believable.
#[test]
fn op_does_not_widen_past_its_widest_necessary_variant() {
    // `RemoveRange( key, lo, hi )` is the widest variant that must carry its
    // payload inline; every other case is a key plus at most one ordinal.
    const NECESSARY_PAYLOAD: usize = size_of::<(u64, u64, u64)>();
    // One word of discriminant, which is what the alignment of these fields
    // costs anyway.
    const BOUND: usize = NECESSARY_PAYLOAD + size_of::<u64>();

    assert!(
        size_of::<Op>() <= BOUND,
        "Op is {} bytes against a {BOUND}-byte bound; a batch pushes ~255 of \
         these per document, so this is an ingest regression, not a layout \
         detail. Box the wide variant's payload instead of raising the bound.",
        size_of::<Op>(),
    );
}
