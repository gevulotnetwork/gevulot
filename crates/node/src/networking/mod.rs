pub mod download_manager;
pub mod p2p;

use eyre::{eyre, Result};
use futures_util::TryStreamExt;
use std::sync::Arc;
use tokio::io::AsyncBufReadExt;
use tokio_util::io::StreamReader;

pub use p2p::P2P;

use crate::storage::Database;

pub struct WhitelistSyncer {
    url: String,
    database: Arc<Database>,
}

impl WhitelistSyncer {
    pub fn new(url: String, database: Arc<Database>) -> Self {
        Self { url, database }
    }

    pub async fn sync(&self) -> Result<()> {
        let url = reqwest::Url::parse(&self.url)?;
        let client = reqwest::ClientBuilder::new().gzip(true).build()?;

        let resp = client.get(url.clone()).send().await?;

        if resp.status() == reqwest::StatusCode::OK {
            let reader = StreamReader::new(
                resp.bytes_stream()
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e)),
            );

            let mut lines = reader.lines();
            let mut key_count = 0;
            while let Ok(Some(line)) = lines.next_line().await {
                let public_key = crate::entity::PublicKey::try_from(line.as_str())?;
                self.database.acl_whitelist(&public_key).await?;
                key_count += 1;
            }

            tracing::info!("{} keys whitelisted", key_count);
            Ok(())
        } else {
            Err(eyre!(
                "failed to download file from {}: response status: {}",
                url,
                resp.status()
            ))
        }
    }
}
