//! Protobuf definitions for the haiiie gRPC service.
//!
//! # Why gRPC rather than Arrow Flight
//!
//! haiiie's remote surface is a query in and a top-k out: a few hundred bytes of
//! request, tens of results. Flight is built for streaming record batches, so it
//! would import Arrow into the server to carry a ten-element answer, and its
//! ticket and endpoint model -- designed so a client can fetch partitions in
//! parallel across nodes -- describes nothing about this workload.
//!
//! Ingest is the one bulk path and it is a client-streaming RPC, which is a
//! shape gRPC has natively.
//!
//! Talking *to* yesnodb is still Arrow Flight, because that is yesnodb's
//! protocol rather than ours.
//!
//! # The boundary
//!
//! What crosses this service is a binary code or residual embedding and a top-k.
//! Chunks, posting lists and
//! accumulator planes do not. That is the same argument that rules out a remote
//! chunk cursor, and it is unaffected by which framing carries the bytes.

/// Generated types for `haiiie.v1`.
pub mod v1 {
    #![allow(
        clippy::doc_markdown,
        clippy::large_enum_variant,
        clippy::derive_partial_eq_without_eq
    )]
    tonic::include_proto!("haiiie.v1");
}

pub use v1::*;
