use eyre::{eyre, Result};
use gevulot_node::types::{
    self,
    transaction::{Payload, ProgramData},
};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::{path::PathBuf, sync::Arc, time::Duration};
use thiserror::Error;
use tokio::{io::AsyncWriteExt, time::sleep};

use crate::{
    cli::Config,
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

        let mut resp = match self.http_client.get(url.clone()).send().await {
            Ok(resp) => resp,
            Err(err) => {
                let uri = file_path
                    .as_path()
                    .components()
                    .rev()
                    .take(2)
                    .map(|c| c.as_os_str().to_os_string())
                    .reduce(|acc, mut el| {
                        el.push("/");
                        el.push(&acc);
                        el
                    })
                    .ok_or_else(|| eyre!("Download bad file path: {:?}", file_path))
                    .and_then(|s| {
                        s.into_string()
                            .map_err(|err| eyre!("Download bad file path: {:?}", file_path))
                    })?;
                let peer_urls: Vec<_> = {
                    let list = self.http_peer_list.read().await;
                    list.iter()
                        .filter_map(|(peer, port)| {
                            port.map(|port| {
                                //use parse to create an URL, no new method.
                                let mut url = reqwest::Url::parse("http://localhost").unwrap(); //unwrap always succeed
                                url.set_ip_host(peer.ip()).unwrap(); //unwrap always succeed
                                url.set_port(Some(port)).unwrap(); //unwrap always succeed
                                url.set_path(&uri); //unwrap always succeed
                                url
                            })
                        })
                        .collect()
                };
                tracing::debug!(
                    "asset manager download file from uri {uri} to  {}, use peer list:{:?}",
                    file_path.as_path().to_str().unwrap().to_string(),
                    peer_urls
                );

                let mut resp = None;
                for url in peer_urls {
                    if let Ok(val) = self.http_client.get(url).send().await {
                        resp = Some(val);
                        break;
                    }
                }
                match resp {
                    Some(resp) => resp,
                    _ => {
                        return Err(eyre!(
                            "Download no host found to download the file: {:?}",
                            file_path
                        ));
                    }
                }
            }
        };

        if resp.status() == reqwest::StatusCode::OK {
            //create a tmp file during download.
            //this way the file won't be available for download from the other nodes
            //until it is completely written.
            let mut tmp_file_path = file_path.clone();
            tmp_file_path.set_extension(".tmp");

            let fd = tokio::fs::File::create(&tmp_file_path).await?;
            let mut fd = tokio::io::BufWriter::new(fd);

            while let Some(chunk) = resp.chunk().await? {
                fd.write_all(&chunk).await?;
            }

            fd.flush().await?;
            //rename to original name
            std::fs::rename(tmp_file_path, file_path)?;
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
