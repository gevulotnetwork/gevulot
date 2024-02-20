pub mod download_manager;
pub mod p2p;

use eyre::{eyre, Result};
use flate2::read::GzDecoder;
use std::{
    io::{BufRead, BufReader},
    sync::Arc,
};

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
        let resp = reqwest::Client::new().get(url.clone()).send().await?;

        if resp.status() == reqwest::StatusCode::OK {
            match resp.bytes().await {
                Ok(bs) => {
                    let mut key_count = 0;

                    // XXX: This is kind of a blocking operation as this
                    // processes the whole whitelist file in one blocking go.
                    let mut decoder = BufReader::new(GzDecoder::new(&bs[..]));
                    let mut buf = String::new();
                    while decoder.read_line(&mut buf).is_ok() {
                        let public_key = crate::entity::PublicKey::try_from(buf.as_str())?;
                        self.database.acl_whitelist(&public_key).await?;
                        buf.clear();
                        key_count += 1;
                    }

                    tracing::info!("{} keys whitelisted", key_count);

                    Ok(())
                }
                Err(err) => Err(eyre!("failed to read server response: {}", err)),
            }
        } else {
            Err(eyre!(
                "failed to download file from {}: response status: {}",
                url,
                resp.status()
            ))
        }
    }
}
