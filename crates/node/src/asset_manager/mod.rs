use eyre::Result;
use std::{path::PathBuf, sync::Arc, time::Duration};
use thiserror::Error;
use tokio::{io::AsyncWriteExt, time::sleep};

use crate::{
    config::Config,
    storage::Database,
    types::{
        transaction::{self, Transaction},
        Program,
    },
};

#[allow(clippy::enum_variant_names)]
#[derive(Error, Debug)]
enum AssetManagerError {
    #[error("program image download")]
    ProgramImageDownload,
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

    async fn process_deployment(&self, _tx: &Transaction) -> Result<()> {
        let prover = Program::default();
        let verifier = Program::default();

        /*
        let mut handles = JoinSet::new();
        handles.spawn(async move { self.process_program(&prover).await });
        handles.spawn(async move { self.process_program(&verifier).await });

        let mut success = true;
        while let Some(res) = handles.join_next().await {
            if res.is_err() {
                success = false;
            }
        }

        if success {
            Ok(())
        } else {
            Err(AssetManagerError::ProgramImageDownload.into())
        }
        */

        self.process_program(&prover).await?;
        self.process_program(&verifier).await?;

        Ok(())
    }

    async fn process_program(&self, program: &Program) -> Result<()> {
        let file_path = PathBuf::new()
            .join(self.config.data_directory.clone())
            .join(program.hash.to_string())
            .join(program.image_file_name.clone());

        let url = reqwest::Url::parse(&program.image_file_url)?;
        self.download(&url, &file_path).await
    }

    async fn process_run(&self, tx: &Transaction) -> Result<()> {
        /*
        let file_name = Path::new(&file.name).file_name().unwrap();
        let file_path = PathBuf::new()
            .join(&self.data_dir)
            .join(file.task_id.to_string())
            .join(file_name);
        */

        todo!();
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

#[cfg(test)]
mod tests {

    #[test]
    fn foobar_test() {
        println!("add test to asset manager");
    }
}