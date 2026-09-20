//! The brute-force scorer every kernel is checked against.
//!
//! # Slow and obviously correct, on purpose
//!
//! This scores every document, sorts, and truncates. It has no pruning, no
//! bit-slicing, no early exit and no planner, because each of those is a thing
//! that could be wrong in the same direction as the implementation it is
//! supposed to check. It exists **before** the kernels -- a differential suite
//! written after an implementation tends to encode that implementation's bugs.
//!
//! It is never deleted because a fast path exists. That is the policy yesnodb
//! records for `ops::generic`, and it is the reason two missing kernel arms were
//! findable there at all: a slow arm and a fast arm return the same answer, so
//! nothing but a deliberate second implementation can see the difference.

use std::collections::BTreeSet;

use haiiie_core::{CodeRef, DocId, Hit, Metric};

use crate::corpus::Corpus;

/// Score every live, admitted document and return the true top-k.
///
/// `live` and `filter` are both sets of corpus indices; a document must be in
/// both. Passing `None` for either admits everything.
///
/// Results are ordered **score descending, then id ascending** -- the same total
/// order [`Hit::rank_key`] defines, so a caller may compare whole vectors rather
/// than multisets. Comparing multisets would not see tie-break drift.
#[must_use]
pub fn top_k(
    corpus: &Corpus,
    query: CodeRef<'_>,
    metric: Metric,
    k: usize,
    live: Option<&BTreeSet<usize>>,
    filter: Option<&BTreeSet<usize>>,
) -> Vec<Hit> {
    let m = query.weight();
    let mut hits: Vec<Hit> = (0..corpus.len())
        .filter(|i| live.is_none_or(|s| s.contains(i)))
        .filter(|i| filter.is_none_or(|s| s.contains(i)))
        .map(|i| {
            let code = corpus.code(i);
            let inter = query.intersect(code);
            let weight = code.weight();
            Hit {
                id: DocId(i as u64),
                score: metric.score(inter, weight, m),
                inter,
                weight,
            }
        })
        .collect();
    hits.sort_by_key(Hit::rank_key);
    hits.truncate(k);
    hits
}
