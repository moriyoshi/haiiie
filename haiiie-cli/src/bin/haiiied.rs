//! `haiiied` -- serves one index over gRPC.
//!
//! Embedded mode opens the directory; peer mode connects to yesnod, which owns
//! the directory, and serves queries through its shared snapshot channel.

use std::io;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use clap::Parser;
use haiiie_core::store::{SetSnapshot, SetStore};
use haiiie_core::{Index, KeySpace, YesnoStore};
use haiiie_grpc::HaiiieService;
use haiiie_peer::PeerStore;
use haiiie_proto::v1::haiiie_server::HaiiieServer;

#[derive(Parser, Debug)]
#[command(name = "haiiied", about = "Serve a haiiie index over gRPC")]
struct Args {
    /// Directory holding the yesnodb data.
    #[arg(
        long,
        conflicts_with = "peer_socket",
        required_unless_present = "peer_socket"
    )]
    data_dir: Option<PathBuf>,
    /// yesnod's served read channel socket. yesnod remains the database owner.
    #[arg(long, conflicts_with = "data_dir")]
    peer_socket: Option<PathBuf>,
    /// yesnod Arrow Flight endpoint for atomic ingest from peer mode.
    #[arg(long, requires = "peer_socket")]
    flight_endpoint: Option<String>,
    /// Bearer-token file for --flight-endpoint; read before each write batch.
    #[arg(long, requires = "flight_endpoint")]
    flight_token_file: Option<PathBuf>,
    /// yesnod control-plane endpoint for durable checkpoints in peer mode.
    #[arg(long, requires = "peer_socket")]
    control_endpoint: Option<String>,
    /// Bearer-token file for --control-endpoint; read on every checkpoint.
    #[arg(long, requires = "control_endpoint")]
    control_token_file: Option<PathBuf>,
    /// Address to listen on.
    #[arg(long, default_value = "127.0.0.1:50071")]
    listen: SocketAddr,
    /// Key-space namespace of the index to serve.
    #[arg(long)]
    namespace: Option<u8>,
    /// Create an index of this width if the namespace is empty.
    #[arg(long)]
    create_dims: Option<u32>,
    /// Rebuild per-block statistics at startup. They tighten the ratio-metric
    /// bounds and are invalidated by every write, so a server started after a
    /// bulk ingest usually wants this.
    #[arg(long)]
    refresh_stats: bool,
    /// Serialized residual model used for float ingest and exact residual search.
    ///
    /// When creating an index, `--create-dims` must be 512 and the model identity
    /// is bound into the new index metadata. Opening a model-bound index without
    /// this option is refused before the server starts accepting requests.
    #[arg(long)]
    residual_model: Option<PathBuf>,
    /// Scan width. Defaults to the host's parallelism; zero or one is serial.
    ///
    /// Worth setting on a large host. Measured on this project's corpora, the
    /// scan gains about **1.3x between four and eight threads** and no more:
    /// twenty threads is no better than serial, because the storage layer takes
    /// a per-shard mutex on every chunk read and the shard count is the ceiling
    /// on concurrent readers. The default is the host's core count, which on a
    /// machine with many cores is above that plateau and was previously not
    /// adjustable -- the service has always had the setter and nothing could
    /// reach it. Residual scoring uses this width for unfiltered indexes with
    /// at least 32,768 live rows and caps that path at eight measured workers;
    /// smaller and filtered residual scans stay serial.
    #[arg(long)]
    threads: Option<usize>,
    /// Storage shards, used only when the data directory is first created.
    ///
    /// This is how many threads can read at once: the storage layer guards each
    /// shard with a mutex and takes it per chunk read. Raising it trades ingest
    /// rate for query parallelism, and it **cannot be changed later** -- the
    /// count is recorded in the directory and reopening ignores this flag. Lower
    /// it for an ingest-heavy index that is queried by one thread.
    #[arg(long, default_value_t = haiiie_core::DEFAULT_SHARDS, conflicts_with = "peer_socket")]
    shards: usize,
}

/// An omitted namespace uses 1 for a fresh directory. Refuse an implicit
/// open or create beside legacy namespace-0 data, where the old default wrote.
fn resolve_namespace<S: SetStore>(store: &S, requested: Option<u8>) -> haiiie_core::Result<u8> {
    let namespace = requested.unwrap_or(1);
    if requested.is_none() {
        let snap = store.snapshot()?;
        if snap.cardinality(KeySpace::new(namespace).meta())? == 0
            && snap.cardinality(KeySpace::new(0).meta())? != 0
        {
            return Err(haiiie_core::Error::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "namespace 0 already contains an index; pass --namespace 0 to open it, or --namespace 1 to explicitly create a separate index",
            )));
        }
    }
    Ok(namespace)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    use tracing_subscriber::EnvFilter;
    use tracing_subscriber::fmt::format::FmtSpan;

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_span_events(FmtSpan::CLOSE)
        .try_init()
        .map_err(|error| std::io::Error::other(error.to_string()))?;

    let args = Args::parse();
    let residual_model = match args.residual_model.as_ref() {
        Some(path) => Some(haiiie_embed::ResidualModel::from_bytes(&std::fs::read(
            path,
        )?)?),
        None => None,
    };
    if let Some(socket) = &args.peer_socket {
        let mut store = PeerStore::new(socket);
        if let Some(endpoint) = &args.flight_endpoint {
            store = store.with_flight_auth(endpoint.clone(), args.flight_token_file.clone())?;
        }
        if let Some(endpoint) = &args.control_endpoint {
            store = store.with_control(endpoint.clone(), args.control_token_file.clone())?;
        }
        let source = socket.display().to_string();
        loop {
            match run(store.clone(), &args, &residual_model, &source).await {
                Err(error) if retryable_peer_open_error(error.as_ref()) => {
                    tracing::info!(%error, "waiting for yesnod to open the peer channel");
                    // At most one retry per second, with at most one second of
                    // extra startup delay after yesnod becomes available.
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
                outcome => break outcome,
            }
        }
    } else {
        let dir = args
            .data_dir
            .as_ref()
            .expect("clap requires one storage source");
        let store = YesnoStore::open_with_shards(dir, args.shards)?;
        run(store, &args, &residual_model, &dir.display().to_string()).await
    }
}

/// The operator binds its channel before opening the database. Only the
/// connection and generation errors that can occur during that gap are retried;
/// malformed metadata, a wrong model, and rejected writes remain hard failures.
fn retryable_peer_open_error(error: &(dyn std::error::Error + 'static)) -> bool {
    let Some(error) = error.downcast_ref::<haiiie_core::Error>() else {
        return false;
    };
    match error {
        haiiie_core::Error::SnapshotExpired { version: 0 } => true,
        haiiie_core::Error::Io(error) => matches!(
            error.kind(),
            io::ErrorKind::NotFound
                | io::ErrorKind::ConnectionRefused
                | io::ErrorKind::NotConnected
                | io::ErrorKind::ConnectionAborted
                | io::ErrorKind::ConnectionReset
                | io::ErrorKind::BrokenPipe
                | io::ErrorKind::UnexpectedEof
        ),
        _ => false,
    }
}

async fn run<S: SetStore + Clone + 'static>(
    store: S,
    args: &Args,
    residual_model: &Option<haiiie_embed::ResidualModel>,
    source: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let namespace = resolve_namespace(&store, args.namespace)?;
    let index = match args.create_dims {
        Some(dims) => {
            if residual_model.is_some() && dims != 512 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "--residual-model requires --create-dims 512",
                )
                .into());
            }
            let created = if let Some(model) = residual_model.as_ref() {
                Index::create_with_model_id(store.clone(), namespace, dims, model.id().as_bytes())
            } else {
                Index::create(store.clone(), namespace, dims)
            };
            match created {
                Ok(index) => index,
                // Already there: open it rather than refusing, so `--create-dims`
                // is safe to leave in a startup command.
                Err(haiiie_core::Error::AlreadyExists(_)) => Index::open(store, namespace)?,
                Err(error) => return Err(error.into()),
            }
        }
        None => Index::open(store, namespace)?,
    };
    if index.meta().model_id.is_some() && residual_model.is_none() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "this index is model-bound; pass --residual-model",
        )
        .into());
    }
    if let Some(model) = residual_model.as_ref() {
        model.require_index_meta(index.meta())?;
    }

    if args.refresh_stats {
        tracing::info!("rebuilding block statistics");
        let version = index.refresh_stats()?;
        tracing::info!(version, "block statistics refreshed");
    }

    let meta = index.meta();
    tracing::info!(
        storage = source,
        namespace,
        code_bits = meta.dims,
        model_bound = meta.model_id.is_some(),
        live_documents = index.len()?,
        listen = %args.listen,
        "serving index"
    );

    tonic::transport::Server::builder()
        .add_service(HaiiieServer::new({
            let mut svc = HaiiieService::new(index);
            if let Some(model) = residual_model {
                svc = svc.with_residual_model(model.clone())?;
            }
            if let Some(n) = args.threads {
                svc = svc.with_threads(n);
            }
            svc
        }))
        .serve_with_shutdown(args.listen, async {
            let _ = tokio::signal::ctrl_c().await;
            tracing::info!("shutdown signal received");
        })
        .await?;
    tracing::info!("server stopped");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use haiiie_core::{CodeRef, DocId, Error};

    #[test]
    fn peer_startup_retries_unavailable_but_not_bad_index_data() {
        for error in [
            Error::SnapshotExpired { version: 0 },
            Error::Io(io::Error::new(
                io::ErrorKind::NotFound,
                "socket not bound yet",
            )),
            Error::Io(io::Error::new(
                io::ErrorKind::ConnectionRefused,
                "yesnod has not accepted the channel",
            )),
        ] {
            assert!(retryable_peer_open_error(&error), "{error}");
        }
        for error in [
            Error::SnapshotExpired { version: 7 },
            Error::CorruptMeta("bad index header"),
            Error::Io(io::Error::new(
                io::ErrorKind::InvalidData,
                "bad channel frame",
            )),
        ] {
            assert!(!retryable_peer_open_error(&error), "{error}");
        }
    }

    #[test]
    fn implicit_startup_refuses_to_strand_namespace_zero() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("db");
        let old =
            Index::create(YesnoStore::open(&path).expect("store"), 0, 64).expect("legacy index");
        let mut writer = old.writer();
        writer.put(DocId(0), CodeRef::Dense(&[1])).expect("put");
        writer.commit().expect("commit");
        drop(old);

        let store = YesnoStore::open(&path).expect("reopen");
        let error = resolve_namespace(&store, None).expect_err("must name old namespace");
        assert!(matches!(&error, Error::Io(e) if e.to_string().contains("--namespace 0")));
        assert_eq!(resolve_namespace(&store, Some(0)).unwrap(), 0);
        assert_eq!(resolve_namespace(&store, Some(1)).unwrap(), 1);
        let error =
            resolve_namespace(&store, None).expect_err("plain open must name old namespace");
        assert!(matches!(&error, Error::Io(e) if e.to_string().contains("--namespace 0")));
        drop(store);

        let old = Index::open(YesnoStore::open(&path).expect("reopen"), 0)
            .expect("old index remains openable");
        assert_eq!(old.len().expect("len"), 1);
    }

    #[test]
    fn implicit_creation_uses_one_when_it_already_exists() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("db");
        let current = Index::create(YesnoStore::open(&path).expect("store"), 1, 64)
            .expect("new default index");
        drop(current);
        let store = YesnoStore::open(&path).expect("reopen");
        assert_eq!(resolve_namespace(&store, None).unwrap(), 1);
    }
}
