use crate::txvalidation::acl::AclWhitelist;
use crate::txvalidation::download_manager;
use crate::txvalidation::EventProcessError;
use crate::types::{
    transaction::{Received, Validated},
    Transaction,
};
use crate::Mempool;
use futures::future::join_all;
use futures_util::TryFutureExt;
use std::fmt::Debug;
use std::net::SocketAddr;
use std::path::Path;
use tokio::sync::mpsc::UnboundedSender;

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
pub struct TxEvent<T: Debug> {
    pub tx: Transaction<Received>,
    pub tx_type: T,
}

impl From<Transaction<Received>> for TxEvent<ReceivedTx> {
    fn from(tx: Transaction<Received>) -> Self {
        TxEvent {
            tx,
            tx_type: ReceivedTx,
        }
    }
}

impl From<TxEvent<ReceivedTx>> for TxEvent<DownloadTx> {
    fn from(event: TxEvent<ReceivedTx>) -> Self {
        TxEvent {
            tx: event.tx,
            tx_type: DownloadTx,
        }
    }
}

impl From<TxEvent<DownloadTx>> for TxEvent<NewTx> {
    fn from(event: TxEvent<DownloadTx>) -> Self {
        TxEvent {
            tx: event.tx,
            tx_type: NewTx,
        }
    }
}

impl From<TxEvent<DownloadTx>> for Option<TxEvent<PropagateTx>> {
    fn from(event: TxEvent<DownloadTx>) -> Self {
        match event.tx.state {
            Received::P2P => None,
            Received::RPC => Some(TxEvent {
                tx: event.tx,
                tx_type: PropagateTx,
            }),
            Received::TXRESULT => Some(TxEvent {
                tx: event.tx,
                tx_type: PropagateTx,
            }),
        }
    }
}

//Processing of event that arrive: SourceTxType.
impl TxEvent<ReceivedTx> {
    pub async fn process_event(
        self,
        acl_whitelist: &impl AclWhitelist,
    ) -> Result<TxEvent<DownloadTx>, EventProcessError> {
        match self.validate_tx(acl_whitelist).await {
            Ok(()) => Ok(self.into()),
            Err(err) => Err(EventProcessError::ValidateError(format!(
                "Tx validation fail:{err}"
            ))),
        }
    }

    //Tx validation process.
    async fn validate_tx(
        &self,
        acl_whitelist: &impl AclWhitelist,
    ) -> Result<(), EventProcessError> {
        self.tx.validate().map_err(|err| {
            EventProcessError::ValidateError(format!("Error during transaction validation:{err}",))
        })?;

        // Secondly verify that author is whitelisted.
        if !acl_whitelist.contains(&self.tx.author).await? {
            return Err(EventProcessError::ValidateError(
                "Tx permission denied signer not authorized".to_string(),
            ));
        }

        Ok(())
    }
}

//Download Tx processing
impl TxEvent<DownloadTx> {
    pub async fn process_event(
        self,
        local_directory_path: &Path,
        http_peer_list: Vec<(SocketAddr, Option<u16>)>,
    ) -> Result<(TxEvent<NewTx>, Option<TxEvent<PropagateTx>>), EventProcessError> {
        let http_client = reqwest::Client::new();
        let asset_file_list = self.tx.get_asset_list().map_err(|err| {
            EventProcessError::DownloadAssetError(format!(
                "Asset file param conversion error:{err}"
            ))
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
            .map_err(|err| {
                tracing::error!("Error during Tx file download:{err}");
                EventProcessError::DownloadAssetError(format!("Exwecution error:{err}"))
            })?;
        let newtx: TxEvent<NewTx> = self.clone().into();
        let propagate: Option<TxEvent<PropagateTx>> = self.into();
        Ok((newtx, propagate))
    }
}

//Propagate Tx processing
impl TxEvent<PropagateTx> {
    pub async fn process_event(
        self,
        p2p_sender: &UnboundedSender<Transaction<Validated>>,
    ) -> Result<(), EventProcessError> {
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
        tracing::info!("Tx validation propagate tx:{}", tx.hash.to_string());
        p2p_sender.send(tx).map_err(|err| Box::new(err).into())
    }
}

impl TxEvent<NewTx> {
    pub async fn process_event(self, mempool: &mut Mempool) -> Result<(), EventProcessError> {
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
        tracing::info!("Tx validation save tx:{}", tx.hash.to_string());
        mempool
            .add(tx)
            .map_err(|err| EventProcessError::SaveTxError(format!("{err}")))
            .await
    }
}
