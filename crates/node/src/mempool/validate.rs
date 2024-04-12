use crate::mempool::download_manager;
use crate::types::{
    transaction::{Received, Validated},
    Transaction,
};
use futures::future::join_all;
use gevulot_node::acl;
use std::collections::HashMap;
use std::fmt::Debug;
use std::net::SocketAddr;
use std::path::Path;
use thiserror::Error;
use tokio::sync::mpsc::UnboundedSender;

const MAX_CACHED_TX_FOR_VERIFICATION: usize = 50;
const MAX_WAITING_TX_FOR_VERIFICATION: usize = 100;
const MAX_WAITING_TIME_IN_MS: u64 = 3600 * 1000; // One hour.

#[allow(clippy::enum_variant_names)]
#[derive(Error, Clone, Debug)]
pub enum ValidateError {
    #[error("Fail to send the Tx on the channel: {0}")]
    PropagateTxError(#[from] Box<tokio::sync::mpsc::error::SendError<Transaction<Validated>>>),
    #[error("validation fail: {0}")]
    TxValidateError(String),
    #[error("Tx asset fail to download because {0}")]
    DownloadAssetError(String),
    #[error("Save Tx error: {0}")]
    SaveTxError(String),
    #[error("AclWhite list authenticate error: {0}")]
    AclWhiteListAuthError(#[from] acl::AclWhiteListError),
}

//event type.
#[derive(Debug, Clone)]
pub struct ReceivedTx;

#[derive(Debug, Clone)]
pub struct DownloadTx;

#[derive(Debug, Clone)]
pub struct NewTx;

#[derive(Debug, Clone)]
pub struct PropagateTx;

//Event processing depends on the marker type.
#[derive(Debug, Clone)]
pub struct TxValidateEvent<T: Debug> {
    pub tx: Transaction<Received>,
    pub tx_type: T,
}

impl From<Transaction<Received>> for TxValidateEvent<ReceivedTx> {
    fn from(tx: Transaction<Received>) -> Self {
        TxValidateEvent {
            tx,
            tx_type: ReceivedTx,
        }
    }
}

impl From<TxValidateEvent<ReceivedTx>> for TxValidateEvent<DownloadTx> {
    fn from(event: TxValidateEvent<ReceivedTx>) -> Self {
        TxValidateEvent {
            tx: event.tx,
            tx_type: DownloadTx,
        }
    }
}

impl From<TxValidateEvent<DownloadTx>> for TxValidateEvent<NewTx> {
    fn from(event: TxValidateEvent<DownloadTx>) -> Self {
        TxValidateEvent {
            tx: event.tx,
            tx_type: NewTx,
        }
    }
}

impl From<TxValidateEvent<DownloadTx>> for Option<TxValidateEvent<PropagateTx>> {
    fn from(event: TxValidateEvent<DownloadTx>) -> Self {
        match event.tx.state {
            Received::P2P => None,
            Received::RPC => Some(TxValidateEvent {
                tx: event.tx,
                tx_type: PropagateTx,
            }),
            Received::TXRESULT => Some(TxValidateEvent {
                tx: event.tx,
                tx_type: PropagateTx,
            }),
        }
    }
}

//Processing of event that arrive: SourceTxType.
impl TxValidateEvent<ReceivedTx> {
    pub async fn verify_tx(
        self,
        acl_whitelist: &impl acl::AclWhitelist,
    ) -> Result<TxValidateEvent<DownloadTx>, ValidateError> {
        match self.validate_tx(acl_whitelist).await {
            Ok(()) => Ok(self.into()),
            Err(err) => Err(ValidateError::TxValidateError(format!(
                "Tx validation fail:{err}"
            ))),
        }
    }

    //Tx validation process.
    async fn validate_tx(
        &self,
        acl_whitelist: &impl acl::AclWhitelist,
    ) -> Result<(), ValidateError> {
        self.tx.validate().map_err(|err| {
            ValidateError::TxValidateError(format!("Error during transaction validation:{err}",))
        })?;

        // Secondly verify that author is whitelisted.
        if !acl_whitelist.contains(&self.tx.author).await? {
            return Err(ValidateError::TxValidateError(
                "Tx permission denied signer not authorized".to_string(),
            ));
        }

        Ok(())
    }
}

//Download Tx processing
impl TxValidateEvent<DownloadTx> {
    pub async fn downlod_tx_assets(
        self,
        local_directory_path: &Path,
        http_peer_list: Vec<(SocketAddr, Option<u16>)>,
    ) -> Result<(TxValidateEvent<NewTx>, Option<TxValidateEvent<PropagateTx>>), ValidateError> {
        let http_client = reqwest::Client::new();
        let asset_file_list = self.tx.get_asset_list().map_err(|err| {
            ValidateError::DownloadAssetError(format!("Asset file param conversion error:{err}"))
        })?;

        let futures: Vec<_> = asset_file_list
            .into_iter()
            .map(|asset_file| {
                download_manager::download_asset_file(
                    local_directory_path,
                    &http_peer_list,
                    &http_client,
                    asset_file,
                )
            })
            .collect();
        join_all(futures)
            .await
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(|err| ValidateError::DownloadAssetError(format!("Execution error:{err}")))?;
        let newtx: TxValidateEvent<NewTx> = self.clone().into();
        let propagate: Option<TxValidateEvent<PropagateTx>> = self.into();
        Ok((newtx, propagate))
    }
}

// Propagate Tx processing.
impl TxValidateEvent<PropagateTx> {
    pub async fn propagate_tx(
        self,
        p2p_sender: &UnboundedSender<Transaction<Validated>>,
    ) -> Result<(), ValidateError> {
        let tx = Transaction {
            author: self.tx.author,
            hash: self.tx.hash,
            payload: self.tx.payload,
            nonce: self.tx.nonce,
            signature: self.tx.signature,
            //TODO should be updated after the p2p send with a notification
            propagated: true,
            executed: self.tx.executed,
            state: Validated,
        };
        tracing::info!(
            "Tx validation propagate tx:{} payload:{}",
            tx.hash.to_string(),
            tx.payload
        );
        p2p_sender.send(tx).map_err(|err| Box::new(err).into())
    }
}

pub async fn convert_peer_list_to_vec(
    http_peer_list: &tokio::sync::RwLock<HashMap<SocketAddr, Option<u16>>>,
) -> Vec<(SocketAddr, Option<u16>)> {
    http_peer_list
        .read()
        .await
        .iter()
        .map(|(a, p)| (*a, *p))
        .collect()
}
