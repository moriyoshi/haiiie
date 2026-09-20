//! **haiiie** -- an exact zero-one vector search engine over yesnodb.
//!
//! Documents are `D`-bit binary codes. haiiie returns the **true top-k** under
//! the binary code distance, composable with arbitrary boolean filters.
//!
//! # What "exact" claims, and what it does not
//!
//! haiiie is an exact solver for an approximate problem. The top-k it returns is
//! the true top-k *of the binary codes*; it is not the true top-k of whatever
//! the codes were quantized from. The difference matters and is the honest form
//! of the claim: quantization error is measurable offline and identical for
//! every query, where a graph index's search error is query-dependent,
//! tuning-dependent, and invisible at query time.
//!
//! # Status
//!
//! M0 through M8 built; not operated anywhere. This crate carries the vocabulary
//! a search speaks -- documents, codes, metrics, exactly comparable scores, the
//! key space -- and the engine that answers with it: three scan paths, two
//! accumulators, all four metrics, filter composition, per-block statistics and
//! a resumable scan. `README.md` owns the honest list of what is and is not
//! built; this block deliberately does not restate it.
//!
//! The brute-force oracle in `haiiie-testkit` is not scaffolding that the
//! kernels replaced. It is what every kernel is still checked against, and the
//! kernels are checked against each other, which is the arrangement that makes
//! a wrong answer hard to ship.

pub mod code;
pub mod compact;
pub mod error;
pub mod index;
pub mod keyspace;
pub mod meta;
pub mod score;
pub mod search;
pub mod slice;
pub mod store;
pub mod yesno_store;

pub use code::{CodeRef, DocId, ORDINAL_MAX};
pub use compact::{Compaction, acknowledge_compaction, compact};
pub use error::{Error, Result};
pub use index::{BlockStats, Index, Writer};
pub use keyspace::{BLOCK_ORDINALS, KeySpace, Kind, block_of};
pub use meta::IndexMeta;
pub use score::{Hit, Metric, Score};
pub use search::{
    Consistency, Filter, Hits, Kernel, Path, PathHint, Plan, RowHit, RowHits, RowScorer, RowSearch,
    ScanStats, Search,
};
pub use slice::{BLOCK_WORDS, BlockMask, Csa, Slice};
pub use store::{Batch, Op, SetSnapshot, SetStore};
pub use yesno_store::{DEFAULT_SHARDS, YesnoSnapshot, YesnoStore};
