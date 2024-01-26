use crate::{
    cli::Config,
    storage::Database,
    types::{
        transaction::{self, Transaction},
        Hash, Program,
    },
};
use eyre::{eyre, Result};
use gevulot_node::types::{
    self,
    transaction::{Payload, ProgramData},
};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::{path::PathBuf, sync::Arc, time::Duration};
use thiserror::Error;
use tokio::time::sleep;

#[allow(clippy::enum_variant_names)]
#[derive(Error, Debug)]
enum AssetManagerError {
    #[error("program image download")]
    ProgramImageDownload,

    #[error("incompatible transaction payload")]
    IncompatibleTxPayload(Hash),
}

/// AssetManager is reponsible for coordinating asset management for a new
/// transaction. New deployment of prover & verifier requires downloading of
/// VM images for those programs. Similarly, Run transaction has associated
/// input data which must be downloaded, but also built into workspace volume
/// for execution.
pub struct AssetManager {
    config: Arc<Config>,
    database: Arc<Database>,
    http_client: reqwest::Client,
    http_peer_list: Arc<tokio::sync::RwLock<HashMap<SocketAddr, Option<u16>>>>,
}

impl AssetManager {
    pub fn new(
        config: Arc<Config>,
        database: Arc<Database>,
        http_peer_list: Arc<tokio::sync::RwLock<HashMap<SocketAddr, Option<u16>>>>,
    ) -> Self {
        AssetManager {
            config,
            database,
            http_client: reqwest::Client::new(),
            http_peer_list,
        }
    }

    pub async fn run(&self) -> Result<()> {
        // Main processing loop.
        loop {
            for tx_hash in self.database.get_incomplete_assets().await? {
                if let Some(tx) = self.database.find_transaction(&tx_hash).await? {
                    if let Err(err) = self.process_transaction(&tx).await {
                        tracing::error!(
                            "failed to process transaction (hash: {}) assets: {}",
                            tx.hash,
                            err
                        );
                        continue;
                    }

                    self.database.mark_asset_complete(&tx_hash).await?;
                } else {
                    tracing::warn!("asset entry for missing transaction; hash: {}", &tx_hash);
                }
            }

            // TODO: Define specific period for Asset processing refresh and
            // compute remaining sleep time from that. If asset processing
            // takes longer than anticipated, then there's no sleep. Otherwise
            // next iteration starts from same periodic cycle as normally.
            sleep(Duration::from_millis(500)).await;
        }
    }

    /// handle_transaction admits transaction into `AssetManager` for further
    /// processing.
    pub async fn handle_transaction(&self, tx: &Transaction) -> Result<()> {
        self.database.add_asset(&tx.hash).await
    }

    async fn process_transaction(&self, tx: &Transaction) -> Result<()> {
        match tx.payload {
            transaction::Payload::Deploy { .. } => self.process_deployment(tx).await,
            transaction::Payload::Run { .. } => self.process_run(tx).await,
            // Other transaction types don't have external assets that would
            // need processing.
            _ => Ok(()),
        }
    }

    async fn process_deployment(&self, tx: &Transaction) -> Result<()> {
        let (prover, verifier) = match tx.payload.clone() {
            Payload::Deploy {
                name: _,
                prover,
                verifier,
            } => (Program::from(prover), Program::from(verifier)),
            _ => return Err(AssetManagerError::IncompatibleTxPayload(tx.hash).into()),
        };

        self.process_program(&prover).await?;
        self.process_program(&verifier).await?;

        Ok(())
    }

    async fn process_program(&self, program: &Program) -> Result<()> {
        self.download_image(
            &program.image_file_url,
            program.hash,
            &program.image_file_name,
            &program.image_file_checksum,
        )
        .await
    }

    async fn process_run(&self, tx: &Transaction) -> Result<()> {
        let workflow = match tx.payload.clone() {
            Payload::Run { workflow } => workflow,
            _ => return Err(AssetManagerError::IncompatibleTxPayload(tx.hash).into()),
        };

        // TODO: Ideally the following would happen concurrently for each file...
        for step in workflow.steps {
            for input in step.inputs {
                match input {
                    ProgramData::Input {
                        file_name,
                        file_url,
                        checksum,
                    } => {
                        let f = types::File {
                            tx: tx.hash,
                            name: file_name,
                            url: file_url,
                        };
                        crate::networking::download_manager::download_file(
                            &f.url,
                            &self.config.data_directory,
                            f.get_file_relative_path()
                                .to_str()
                                .ok_or(eyre!("Download bad file path: {:?}", f.name))?,
                            self.get_peer_list().await,
                            &self.http_client,
                            checksum.into(),
                        )
                        .await?;
                    }
                    ProgramData::Output { .. } => {
                        /* ProgramData::Output asinput means it comes from another
                        program execution -> skip this branch. */
                    }
                }
            }
        }

        Ok(())
    }

    /// download downloads file from the given `url` and saves it to file in `file_path`.
    async fn download_image(
        &self,
        url: &str,
        program_hash: gevulot_node::types::Hash,
        image_file_name: &str,
        file_checksum: &str,
    ) -> Result<()> {
        let file_path = PathBuf::new()
            .join("images")
            .join(program_hash.to_string())
            .join(image_file_name);
        tracing::info!(
            "asset download url:{url} file_path:{file_path:?} file_checksum:{file_checksum}"
        );
        crate::networking::download_manager::download_file(
            url,
            &self.config.data_directory,
            file_path
                .to_str()
                .ok_or(eyre!("Download bad file path: {:?}", file_path))?,
            self.get_peer_list().await,
            &self.http_client,
            file_checksum.into(),
        )
        .await
    }

    async fn get_peer_list(&self) -> Vec<(SocketAddr, Option<u16>)> {
        self.http_peer_list
            .read()
            .await
            .iter()
            .map(|(a, p)| (*a, *p))
            .collect()
    }
}
