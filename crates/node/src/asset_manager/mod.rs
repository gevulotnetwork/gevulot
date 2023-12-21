use eyre::Result;
use gevulot_node::types::{
    self,
    transaction::{Payload, ProgramData},
};
use std::{path::PathBuf, sync::Arc, time::Duration};
use thiserror::Error;
use tokio::{io::AsyncWriteExt, time::sleep};

use crate::{
    config::Config,
    storage::{self, Database},
    types::{
        transaction::{self, Transaction},
        Hash, Program,
    },
};

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
}

impl AssetManager {
    pub fn new(config: Arc<Config>, database: Arc<Database>) -> Self {
        AssetManager {
            config,
            database,
            http_client: reqwest::Client::new(),
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
            _ => return Err(AssetManagerError::IncompatibleTxPayload(tx.hash.clone()).into()),
        };

        self.process_program(&prover).await?;
        self.process_program(&verifier).await?;

        Ok(())
    }

    async fn process_program(&self, program: &Program) -> Result<()> {
        // TODO: Process and program files are now downloaded in different ways. Combine these.
        let file_path = PathBuf::new()
            .join(self.config.data_directory.clone())
            .join("images")
            .join(program.hash.to_string())
            .join(program.image_file_name.clone());

        let url = reqwest::Url::parse(&program.image_file_url)?;
        self.download(&url, &file_path).await
    }

    async fn process_run(&self, tx: &Transaction) -> Result<()> {
        // TODO: Process and program files are now downloaded in different ways. Combine these.
        let file_storage = storage::File::new(&self.config.data_directory);

        let workflow = match tx.payload.clone() {
            Payload::Run { workflow } => workflow,
            _ => return Err(AssetManagerError::IncompatibleTxPayload(tx.hash.clone()).into()),
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
                            tx: tx.hash.clone(),
                            name: file_name,
                            url: file_url,
                        };
                        file_storage.download(&f).await?;
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
    async fn download(&self, url: &reqwest::Url, file_path: &PathBuf) -> Result<()> {
        // TODO: Blocking operation.
        std::fs::create_dir_all(file_path.parent().unwrap())?;

        let mut resp = self.http_client.get(url.clone()).send().await?;

        if resp.status() == reqwest::StatusCode::OK {
            let fd = tokio::fs::File::create(&file_path).await?;
            let mut fd = tokio::io::BufWriter::new(fd);

            while let Some(chunk) = resp.chunk().await? {
                fd.write_all(&chunk).await?;
            }

            fd.flush().await?;
            tracing::info!(
                "downloaded file to {}",
                file_path.as_path().to_str().unwrap().to_string()
            );
        } else {
            tracing::error!(
                "failed to download file from {}: response status: {}",
                url,
                resp.status()
            );
        }

        Ok(())
    }
}
