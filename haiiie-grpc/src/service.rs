//! The gRPC service boundary.
//!
//! yesnodb takes an exclusive directory lock, so a process holds one writable
//! index. Searches take independent snapshots, while streamed ingest is
//! serialized here at the boundary where concurrent writers arrive.
//! Model-bound ingest encodes bounded chunks in parallel, then packs only
//! complete aligned runs of fresh rows into forward chunk masks. A bounded
//! producer reads and encodes the next chunk while the single ordered writer
//! applies the current one. Every other mutation keeps the point path. A packed
//! run commits with its LIVE and ATTR updates before the next run, preserving
//! each document's atomicity when a stream fails.

use std::sync::Arc;

use haiiie_core::{CodeRef, DocId, Index, SetStore};
use haiiie_proto::v1::{self, haiiie_server::Haiiie};
use rayon::prelude::*;
use tokio::sync::Mutex;
use tonic::{Request, Response, Status, Streaming};

use crate::convert::{self, OwnedQuery};

/// Serves one index.
pub struct HaiiieService<S: SetStore> {
    index: Arc<Index<S>>,
    write: Mutex<()>,
    threads: usize,
    residual_model: Option<Arc<haiiie_embed::ResidualModel>>,
}

impl<S: SetStore> HaiiieService<S> {
    /// Wrap an open index.
    pub fn new(index: Index<S>) -> Self {
        Self {
            index: Arc::new(index),
            write: Mutex::new(()),
            // The store locks each shard per chunk read, so the shard count is
            // the useful ceiling even when more cores are available.
            threads: std::thread::available_parallelism().map_or(1, std::num::NonZero::get),
            residual_model: None,
        }
    }

    /// Attach the exact model required by a model-bound residual index.
    pub fn with_residual_model(
        mut self,
        model: haiiie_embed::ResidualModel,
    ) -> Result<Self, haiiie_embed::CodecError> {
        model.require_index_meta(self.index.meta())?;
        self.residual_model = Some(Arc::new(model));
        Ok(self)
    }

    #[allow(
        clippy::result_large_err,
        reason = "the service boundary returns tonic Status"
    )]
    fn required_residual_model(&self) -> Result<&haiiie_embed::ResidualModel, Status> {
        self.residual_model.as_deref().ok_or_else(|| {
            Status::failed_precondition("this residual index has no matching loaded model")
        })
    }

    /// Override the scan width. Zero or one is serial.
    #[must_use]
    pub fn with_threads(mut self, threads: usize) -> Self {
        self.threads = threads.max(1);
        self
    }

    /// The index being served.
    pub fn index(&self) -> &Index<S> {
        &self.index
    }
}

// On 8,192 COCO-512 documents with the fitted model, a 1,024-row batch took
// 0.133 s with 20 Rayon workers versus 0.142 s at 256 rows (2026-09-23
// encoder sweep). It bounds the encoded output to 64 KiB. The input message is
// already owned by tonic; returning mutations beside codes preserves order.
const ENCODE_CHUNK: usize = 1024;

type EncodedChunk = (
    Vec<v1::Mutation>,
    Vec<Result<Option<[u64; 8]>, haiiie_embed::CodecError>>,
);

// A dropped ingest call must not leave its stream reader waiting indefinitely
// for a client that will never send another message.
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl AbortOnDrop {
    async fn finish(mut self) -> Result<(), tokio::task::JoinError> {
        (&mut self.0).await
    }
}

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn encode_ingest_chunk(
    mutations: Vec<v1::Mutation>,
    model: Arc<haiiie_embed::ResidualModel>,
) -> Result<EncodedChunk, Status> {
    let (mutations, codes) = tokio::task::spawn_blocking(move || {
        let codes: Vec<_> = mutations
            .par_iter()
            .map(|mutation| match mutation.operation.as_ref() {
                Some(v1::mutation::Operation::Put(document)) => match document.value.as_ref() {
                    Some(v1::document::Value::Embedding(embedding)) => model
                        .encode_document(&embedding.values)
                        .map(|code| Some(code.to_words())),
                    _ => Ok(None),
                },
                _ => Ok(None),
            })
            .collect();
        (mutations, codes)
    })
    .await
    .map_err(|error| Status::internal(format!("document encoder worker failed: {error}")))?;
    Ok((mutations, codes))
}

async fn produce_ingest_chunks(
    mut stream: Streaming<v1::IngestRequest>,
    model: Option<Arc<haiiie_embed::ResidualModel>>,
    tx: tokio::sync::mpsc::Sender<Result<EncodedChunk, Status>>,
) {
    loop {
        let message = match stream.message().await {
            Ok(Some(message)) => message,
            Ok(None) => return,
            Err(status) => {
                let _ = tx.send(Err(status)).await;
                return;
            }
        };
        let mut pending = message.mutations.into_iter();
        loop {
            let mutations: Vec<_> = pending.by_ref().take(ENCODE_CHUNK).collect();
            if mutations.is_empty() {
                break;
            }
            let has_embeddings = mutations.iter().any(|mutation| {
                matches!(
                    mutation.operation.as_ref(),
                    Some(v1::mutation::Operation::Put(v1::Document {
                        value: Some(v1::document::Value::Embedding(_)),
                        ..
                    }))
                )
            });
            let encoded = if has_embeddings {
                let Some(model) = model.as_ref() else {
                    let _ = tx
                        .send(Err(Status::failed_precondition(
                            "this residual index has no matching loaded model",
                        )))
                        .await;
                    return;
                };
                encode_ingest_chunk(mutations, Arc::clone(model)).await
            } else {
                let codes = (0..mutations.len()).map(|_| Ok(None)).collect();
                Ok((mutations, codes))
            };
            if tx.send(encoded).await.is_err() {
                return;
            }
        }
    }
}

fn to_status(error: haiiie_core::Error) -> Status {
    use haiiie_core::Error as E;
    let status = if error.is_snapshot_expired() {
        Status::aborted(error.to_string())
    } else {
        match error {
            E::DimensionMismatch { .. } | E::ReservedDocId(_) | E::AttrTermTooLarge { .. } => {
                Status::invalid_argument(error.to_string())
            }
            E::AlreadyExists(_) => Status::already_exists(error.to_string()),
            E::BinarySearchOnResidualIndex => Status::failed_precondition(error.to_string()),
            E::CorruptMeta(_) => Status::data_loss(error.to_string()),
            other => Status::internal(other.to_string()),
        }
    };
    match status.code() {
        tonic::Code::Internal | tonic::Code::DataLoss => {
            tracing::error!(code = ?status.code(), error = %status.message(), "request failed");
        }
        tonic::Code::Aborted => {
            tracing::warn!(code = ?status.code(), error = %status.message(), "request interrupted");
        }
        _ => {
            tracing::debug!(code = ?status.code(), error = %status.message(), "request rejected");
        }
    }
    status
}

fn wire_stats(
    scored: u64,
    versions: (u64, u64),
    blocks_visited: u64,
    blocks_skipped: u64,
    resumes: u32,
) -> v1::SearchStats {
    v1::SearchStats {
        documents_scored: scored,
        versions: Some(v1::VersionRange {
            minimum: versions.0,
            maximum: versions.1,
        }),
        blocks_visited,
        blocks_skipped,
        resumes,
    }
}

impl<S: SetStore + 'static> HaiiieService<S> {
    async fn ingest_inner(
        &self,
        request: Request<Streaming<v1::IngestRequest>>,
        started: std::time::Instant,
    ) -> Result<Response<v1::IngestResponse>, Status> {
        let stream = request.into_inner();
        let _writer = self.write.lock().await;
        // One queued chunk can overlap the current writer chunk and the next
        // producer chunk. Their 1,024-row code vectors hold up to 192 KiB
        // combined; the writer also copies a tile into (DocId, code) rows,
        // and tonic owns the input message separately.
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let producer = AbortOnDrop(tokio::spawn(produce_ingest_chunks(
            stream,
            self.residual_model.clone(),
            tx,
        )));

        // This threshold was measured at about 3 GiB per 262,144 unflushed
        // documents in the point-write layout. Check it after each completed
        // chunk so a large stream message cannot bypass the batch bound.
        const FLUSH_AT: usize = 4_000_000;
        let residual_index = self.index.meta().model_id.is_some();
        let mut writer = self.index.writer();
        let (mut written, mut deleted) = (0u64, 0u64);
        let (mut chunks, mut packed_tiles) = (0u64, 0u64);
        let mut last_version = 0u64;

        while let Some(chunk) = rx.recv().await {
            let (mutations, codes) = chunk?;
            chunks += 1;
            // A complete aligned run can stage native FWD chunk masks
            // while its LIVE and ATTR writes stay in the same transaction.
            // Mixed, repeated, sparse-ID, and overwrite runs use the
            // ordinary writer, which remains the exact mutation oracle.
            let tile_rows: Option<Vec<_>> = if residual_index {
                mutations
                    .iter()
                    .zip(&codes)
                    .map(
                        |(mutation, encoded)| match (mutation.operation.as_ref(), encoded) {
                            (Some(v1::mutation::Operation::Put(document)), Ok(Some(words)))
                                if matches!(
                                    document.value.as_ref(),
                                    Some(v1::document::Value::Embedding(_))
                                ) =>
                            {
                                DocId::new(document.id).ok().map(|id| (id, *words))
                            }
                            _ => None,
                        },
                    )
                    .collect()
            } else {
                None
            };
            if let Some(tile_rows) = tile_rows
                && writer
                    .try_put_residual_tile(&tile_rows)
                    .map_err(to_status)?
            {
                packed_tiles += 1;
                for mutation in &mutations {
                    let Some(v1::mutation::Operation::Put(document)) = mutation.operation.as_ref()
                    else {
                        unreachable!("the packed tile contains only puts")
                    };
                    for &term in &document.terms {
                        writer.attr(DocId(document.id), term).map_err(to_status)?;
                    }
                    written += 1;
                }
                // Upstream's 96,903-row tile sweep placed the fsync knee at
                // 1,024-2,048 rows; one encoded chunk is 1,024 rows here.
                // Publishing now also bounds packed-mask memory and keeps
                // all of this tile's LIVE/ATTR operations atomic with FWD.
                if let Some(version) = writer.flush_if_large(0).map_err(to_status)? {
                    last_version = version;
                    tracing::debug!(version, written, deleted, "ingest prefix committed");
                }
                continue;
            }
            for (mutation, encoded) in mutations.iter().zip(codes) {
                match mutation.operation.as_ref() {
                    Some(v1::mutation::Operation::Put(document)) => {
                        let id = DocId::new(document.id).map_err(to_status)?;
                        match document.value.as_ref() {
                            Some(v1::document::Value::BinaryCode(code)) => {
                                if residual_index {
                                    return Err(Status::failed_precondition(
                                        "a binary document cannot be written to a residual index",
                                    ));
                                }
                                let code = convert::code(Some(code))?;
                                writer.put(id, code.as_ref()).map_err(to_status)?;
                            }
                            Some(v1::document::Value::Embedding(_)) => {
                                let encoded = encoded
                                    .map_err(|error| Status::invalid_argument(error.to_string()))?;
                                let code = encoded.ok_or_else(|| {
                                    Status::internal("residual document was not encoded")
                                })?;
                                writer.put(id, CodeRef::Dense(&code)).map_err(to_status)?;
                            }
                            None => {
                                return Err(Status::invalid_argument("document value is missing"));
                            }
                        }
                        for &term in &document.terms {
                            writer.attr(id, term).map_err(to_status)?;
                        }
                        written += 1;
                    }
                    Some(v1::mutation::Operation::Delete(id)) => {
                        writer
                            .delete(DocId::new(*id).map_err(to_status)?)
                            .map_err(to_status)?;
                        deleted += 1;
                    }
                    None => {
                        return Err(Status::invalid_argument("mutation operation is missing"));
                    }
                }
            }
            if let Some(version) = writer.flush_if_large(FLUSH_AT).map_err(to_status)? {
                last_version = version;
                tracing::debug!(version, written, deleted, "ingest prefix committed");
            }
        }
        producer
            .finish()
            .await
            .map_err(|error| Status::internal(format!("ingest producer failed: {error}")))?;

        let version = writer.commit().map_err(to_status)?.max(last_version);
        tracing::info!(
            version,
            written,
            deleted,
            chunks,
            packed_tiles,
            elapsed_ms = started.elapsed().as_millis(),
            "ingest completed"
        );
        Ok(Response::new(v1::IngestResponse {
            version,
            documents_written: written,
            documents_deleted: deleted,
        }))
    }
}

#[tonic::async_trait]
impl<S: SetStore + 'static> Haiiie for HaiiieService<S> {
    #[tracing::instrument(level = "debug", skip_all, fields(limit = request.get_ref().limit))]
    async fn search(
        &self,
        request: Request<v1::SearchRequest>,
    ) -> Result<Response<v1::SearchResponse>, Status> {
        let request = request.into_inner();
        let filter = convert::filter(request.filter.as_ref())?;
        let consistency = convert::consistency(request.scan.as_ref())?;

        match convert::query(request.query.as_ref())? {
            OwnedQuery::Residual(embedding) => {
                let model = self.required_residual_model()?;
                let prepared = model
                    .prepare_query(&embedding)
                    .map_err(|error| Status::invalid_argument(error.to_string()))?;
                let hits = prepared
                    .search(&self.index)
                    .map_err(|error| Status::failed_precondition(error.to_string()))?
                    .k(request.limit as usize)
                    .filter(filter)
                    .consistency(consistency)
                    .threads(self.threads)
                    .execute()
                    .map_err(to_status)?;
                let stats = wire_stats(
                    hits.scored,
                    hits.version_range,
                    hits.stats.blocks_visited,
                    hits.stats.blocks_skipped,
                    hits.stats.resumes,
                );
                tracing::debug!(
                    query_kind = "residual",
                    returned = hits.hits.len(),
                    scored = hits.scored,
                    blocks_visited = hits.stats.blocks_visited,
                    blocks_skipped = hits.stats.blocks_skipped,
                    resumes = hits.stats.resumes,
                    version_min = hits.version_range.0,
                    version_max = hits.version_range.1,
                    "search completed"
                );
                Ok(Response::new(v1::SearchResponse {
                    hits: hits.hits.iter().map(convert::residual_hit).collect(),
                    stats: Some(stats),
                }))
            }
            OwnedQuery::Binary { code, metric } => {
                if self.index.meta().model_id.is_some() {
                    return Err(Status::failed_precondition(
                        "a binary query cannot search a residual index",
                    ));
                }
                let hits = self
                    .index
                    .search()
                    .code(code.as_ref())
                    .metric(metric)
                    .k(request.limit as usize)
                    .filter(filter)
                    .consistency(consistency)
                    .threads(self.threads)
                    .execute()
                    .map_err(to_status)?;
                let stats = wire_stats(
                    hits.scored,
                    hits.version_range,
                    hits.stats.blocks_visited,
                    hits.stats.blocks_skipped,
                    hits.stats.resumes,
                );
                tracing::debug!(
                    query_kind = "binary",
                    returned = hits.hits.len(),
                    scored = hits.scored,
                    blocks_visited = hits.stats.blocks_visited,
                    blocks_skipped = hits.stats.blocks_skipped,
                    resumes = hits.stats.resumes,
                    version_min = hits.version_range.0,
                    version_max = hits.version_range.1,
                    "search completed"
                );
                Ok(Response::new(v1::SearchResponse {
                    hits: hits.hits.iter().map(convert::binary_hit).collect(),
                    stats: Some(stats),
                }))
            }
        }
    }

    #[tracing::instrument(level = "debug", skip_all, fields(limit = request.get_ref().limit))]
    async fn explain(
        &self,
        request: Request<v1::ExplainRequest>,
    ) -> Result<Response<v1::ExplainResponse>, Status> {
        let request = request.into_inner();
        let filter = convert::filter(request.filter.as_ref())?;

        match convert::query(request.query.as_ref())? {
            OwnedQuery::Residual(embedding) => {
                let model = self.required_residual_model()?;
                model
                    .prepare_query(&embedding)
                    .map_err(|error| Status::invalid_argument(error.to_string()))?;
                let blocks = self.index.live_blocks().map_err(to_status)?;
                Ok(Response::new(v1::ExplainResponse {
                    path: "Gather".to_string(),
                    path_reason: "residual scoring reads one stored row per admitted document"
                        .to_string(),
                    kernel: "ResidualLut".to_string(),
                    blocks,
                    exactness: "exact signed-integer ranking key".to_string(),
                    plan: Some(v1::explain_response::Plan::Residual(v1::ResidualPlan {
                        embedding_dimensions: model.dims(),
                        code_bits: self.index.meta().dims,
                    })),
                }))
            }
            OwnedQuery::Binary { code, metric } => {
                if self.index.meta().model_id.is_some() {
                    return Err(Status::failed_precondition(
                        "a binary query cannot explain a residual index",
                    ));
                }
                let plan = self
                    .index
                    .search()
                    .code(code.as_ref())
                    .metric(metric)
                    .k(request.limit as usize)
                    .filter(filter)
                    .explain()
                    .map_err(to_status)?;
                Ok(Response::new(v1::ExplainResponse {
                    path: format!("{:?}", plan.path),
                    path_reason: plan.path_reason.to_string(),
                    kernel: format!("{:?}", plan.kernel),
                    blocks: plan.blocks,
                    exactness: plan.exactness.to_string(),
                    plan: Some(v1::explain_response::Plan::Binary(v1::BinaryPlan {
                        query_bits: plan.query_bits,
                        code_bits: plan.dims,
                        accumulator_levels: plan.accumulator_levels as u32,
                        blocks_with_stats: plan.blocks_with_stats,
                        block_weight_spread: plan.block_weight_spread,
                    })),
                }))
            }
        }
    }

    #[tracing::instrument(level = "debug", skip_all)]
    async fn count(
        &self,
        request: Request<v1::CountRequest>,
    ) -> Result<Response<v1::CountResponse>, Status> {
        let request = request.into_inner();
        let count = self
            .index
            .search()
            .filter(convert::filter(request.filter.as_ref())?)
            .count()
            .map_err(to_status)?;
        tracing::debug!(count, "count completed");
        Ok(Response::new(v1::CountResponse { count }))
    }

    #[tracing::instrument(level = "debug", skip_all)]
    async fn ingest(
        &self,
        request: Request<Streaming<v1::IngestRequest>>,
    ) -> Result<Response<v1::IngestResponse>, Status> {
        let started = std::time::Instant::now();
        let result = self.ingest_inner(request, started).await;
        if let Err(status) = &result {
            tracing::warn!(
                code = ?status.code(),
                error = %status.message(),
                elapsed_ms = started.elapsed().as_millis(),
                "ingest failed; an earlier prefix may have committed"
            );
        }
        result
    }

    #[tracing::instrument(level = "debug", skip_all)]
    async fn describe(
        &self,
        _request: Request<v1::DescribeRequest>,
    ) -> Result<Response<v1::DescribeResponse>, Status> {
        let meta = self.index.meta();
        let (blocks, blocks_with_stats) = if meta.is_residual() {
            (self.index.live_blocks().map_err(to_status)?, 0)
        } else {
            let empty: [u64; 0] = [];
            let plan = self
                .index
                .search()
                .code(CodeRef::Dense(&empty))
                .metric(haiiie_core::Metric::Dot)
                .explain()
                .map_err(to_status)?;
            (plan.blocks, plan.blocks_with_stats)
        };
        let index = match meta.model_id {
            Some(model_id) => v1::describe_response::Index::Residual(v1::ResidualIndex {
                code_bits: meta.dims,
                embedding_dimensions: self.residual_model.as_ref().map(|model| model.dims()),
                model_id: model_id.to_vec(),
            }),
            None => v1::describe_response::Index::Binary(v1::BinaryIndex {
                code_bits: meta.dims,
            }),
        };
        Ok(Response::new(v1::DescribeResponse {
            live_documents: self.index.len().map_err(to_status)?,
            blocks,
            blocks_with_stats,
            index: Some(index),
        }))
    }
}
