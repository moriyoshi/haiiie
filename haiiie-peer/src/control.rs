//! The optional checkpoint worker calls yesnod's authorized ControlPlane RPC.
//! It owns a runtime because SetStore::flush is synchronous and may run inside
//! haiiie's async gRPC handler. A fresh channel per call follows endpoint and
//! credential changes; a failed checkpoint is surfaced, never asserted durable.

use std::io;
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;

use haiiie_core::{Error, Result};
use tonic::Request;
use tonic::metadata::AsciiMetadataValue;
use tonic::transport::Endpoint;

#[derive(Clone, PartialEq, prost::Message)]
struct CheckpointRequest {}

#[derive(Clone, PartialEq, prost::Message)]
struct CheckpointResponse {
    #[prost(uint64, tag = "1")]
    watermark: u64,
}

#[derive(Debug)]
pub(super) struct ControlWorker {
    tx: mpsc::Sender<mpsc::Sender<Result<u64>>>,
}

impl ControlWorker {
    pub(super) fn start(endpoint: String, token_file: Option<PathBuf>) -> Result<Self> {
        let (tx, rx) = mpsc::channel::<mpsc::Sender<Result<u64>>>();
        thread::Builder::new()
            .name("haiiie-control-checkpoint".into())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        for answer in rx {
                            let _ =
                                answer.send(Err(Error::Io(io::Error::other(error.to_string()))));
                        }
                        return;
                    }
                };
                for answer in rx {
                    let result = runtime.block_on(checkpoint(&endpoint, token_file.as_ref()));
                    let _ = answer.send(result);
                }
            })
            .map_err(Error::Io)?;
        Ok(Self { tx })
    }

    pub(super) fn checkpoint(&self) -> Result<u64> {
        let (answer, receive) = mpsc::channel();
        self.tx.send(answer).map_err(|_| {
            Error::Io(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "checkpoint worker stopped",
            ))
        })?;
        receive.recv().map_err(|_| {
            Error::Io(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "checkpoint worker stopped before replying",
            ))
        })?
    }
}

async fn checkpoint(endpoint: &str, token_file: Option<&PathBuf>) -> Result<u64> {
    let channel = Endpoint::from_shared(endpoint.to_owned())
        .map_err(|error| Error::Io(io::Error::new(io::ErrorKind::InvalidInput, error)))?
        .connect()
        .await
        .map_err(|error| Error::Io(io::Error::other(error)))?;
    let mut client = tonic::client::Grpc::new(channel);
    client
        .ready()
        .await
        .map_err(|error| Error::Io(io::Error::other(error)))?;
    let mut request = Request::new(CheckpointRequest {});
    if let Some(path) = token_file {
        let token = std::fs::read_to_string(path)?;
        let value: AsciiMetadataValue = format!("Bearer {}", token.trim())
            .parse()
            .map_err(|error| Error::Io(io::Error::new(io::ErrorKind::InvalidInput, error)))?;
        request.metadata_mut().insert("authorization", value);
    }
    let response: tonic::Response<CheckpointResponse> = client
        .unary(
            request,
            tonic::codegen::http::uri::PathAndQuery::from_static(
                "/yesno.control.v1.ControlPlane/Checkpoint",
            ),
            tonic_prost::ProstCodec::default(),
        )
        .await
        .map_err(|error| Error::Io(io::Error::other(error)))?;
    let watermark = response.into_inner().watermark;
    tracing::info!(watermark, "yesnod checkpoint complete");
    Ok(watermark)
}
