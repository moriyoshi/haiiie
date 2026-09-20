//! Ordered writes use yesnod's existing Flight `apply`, not the lane channel.
//!
//! The `SetStore` interface is synchronous, including when called by an async
//! gRPC handler. A dedicated thread owns the Tokio runtime so a caller never
//! nests `block_on` inside an existing runtime. One worker also preserves batch
//! order for clients sharing this store. A lost apply reply is indeterminate:
//! stop accepting writes until the owner reconciles rather than replaying into
//! a possibly changed database.

use std::io;
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;

use haiiie_core::store::Version;
use haiiie_core::{Error, Result};
use yesno_flight::client::{Mutation, YesnoClient};

enum Job {
    Apply(Vec<Mutation>, mpsc::Sender<Result<Version>>),
}

#[derive(Debug)]
pub(super) struct WriteWorker {
    tx: mpsc::Sender<Job>,
}

impl WriteWorker {
    pub(super) fn start(endpoint: String, token_file: Option<PathBuf>) -> Result<Self> {
        let (tx, rx) = mpsc::channel();
        thread::Builder::new()
            .name("haiiie-flight-writer".into())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        for Job::Apply(_, answer) in rx {
                            let _ = answer.send(Err(Error::Io(io::Error::other(error.to_string()))));
                        }
                        return;
                    }
                };
                let mut client = None;
                let mut indeterminate = false;
                for Job::Apply(mutations, answer) in rx {
                    let was_indeterminate = indeterminate;
                    let result = if indeterminate {
                        Err(Error::Io(io::Error::other(
                            "a prior Flight commit outcome is indeterminate; reopen after reconciliation",
                        )))
                    } else {
                        runtime.block_on(async {
                            if client.is_none() {
                                client = Some(YesnoClient::connect(endpoint.clone()).await.map_err(|error| {
                                    Error::Io(io::Error::new(io::ErrorKind::NotConnected, error.to_string()))
                                })?);
                            }
                            if let Some(path) = token_file.as_ref() {
                                let token = std::fs::read_to_string(path)?;
                                client.as_mut().expect("connected above").inner_mut()
                                    .add_header("authorization", &format!("Bearer {}", token.trim()))
                                    .map_err(|error| Error::Io(io::Error::new(io::ErrorKind::InvalidInput, error.to_string())))?;
                            }
                            let expected = mutations.len() as u64;
                            let ack = client.as_mut().expect("connected above").apply(mutations).await;
                            match ack {
                                Ok(ack) if ack.rows == expected => ack.version.ok_or_else(|| {
                                    indeterminate = true;
                                    Error::Io(io::Error::other("Flight committed but omitted the version"))
                                }),
                                Ok(ack) => {
                                    indeterminate = true;
                                    Err(Error::Io(io::Error::other(format!(
                                        "Flight acknowledged {} mutations after {expected} were sent; outcome is indeterminate",
                                        ack.rows
                                    ))))
                                }
                                Err(error) => {
                                    indeterminate = true;
                                    Err(Error::Io(io::Error::other(format!(
                                        "Flight apply failed; commit outcome is indeterminate: {error}"
                                    ))))
                                }
                            }
                        })
                    };
                    if !was_indeterminate && indeterminate
                        && let Err(error) = &result
                    {
                        tracing::error!(error = %error, "Flight commit outcome is indeterminate; peer writes stopped");
                    }
                    let _ = answer.send(result);
                }
            })
            .map_err(Error::Io)?;
        Ok(Self { tx })
    }

    pub(super) fn apply(&self, mutations: Vec<Mutation>) -> Result<Version> {
        let (answer, receive) = mpsc::channel();
        self.tx.send(Job::Apply(mutations, answer)).map_err(|_| {
            Error::Io(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "Flight writer stopped",
            ))
        })?;
        receive.recv().map_err(|_| {
            Error::Io(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "Flight writer stopped before acknowledging the batch",
            ))
        })?
    }
}
