//! The `haiiied` gRPC service and its Rust client.
//!
//! # What crosses this boundary
//!
//! A binary code or float residual embedding goes in and a top-k comes out.
//! Chunks, posting lists and accumulator planes do not.
//! That is the same reasoning that rules out a remote chunk cursor: shipping raw
//! containers per chunk per query dimension would move hundreds of megabytes
//! per query, and no framing makes that a good idea.
//!
//! # Nothing here is in the engine's dependency budget
//!
//! `haiiie-core` has two direct dependencies and must not acquire tokio, tonic
//! or a transport stack. They live here, and the engine does not name them.

pub mod convert;
pub mod service;

pub use haiiie_proto::v1;
pub use service::HaiiieService;

/// The generated client, re-exported so a caller needs one crate.
pub use haiiie_proto::v1::haiiie_client::HaiiieClient;
