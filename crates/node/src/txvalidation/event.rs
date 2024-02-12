use crate::txvalidation::download_manager;
use crate::txvalidation::EventProcessError;
use crate::types::{
    transaction::{TransactionError, TxReceive, TxValdiated},
    Transaction,
};
use crate::Mempool;
use futures::future::join_all;
use futures_util::TryFutureExt;
use gevulot_node::types::transaction::AclWhitelist;
use std::fmt::Debug;
use std::net::SocketAddr;
use std::path::PathBuf;
use tokio::sync::mpsc::UnboundedSender;

//event type.
#[derive(Debug, Clone)]
pub struct RcvTx;

#[derive(Debug, Clone)]
pub struct DownloadTx;

#[derive(Debug, Clone)]
pub struct NewTx;

#[derive(Debug, Clone)]
pub struct PropagateTx;

//Event processing depends on the marker type.
#[derive(Debug, Clone)]
pub struct EventTx<T: Debug> {
    pub tx: Transaction<TxReceive>,
    pub tx_type: T,
}

impl From<Transaction<TxReceive>> for EventTx<RcvTx> {
    fn from(tx: Transaction<TxReceive>) -> Self {
        EventTx {
            tx: tx,
            tx_type: RcvTx,
        }
    }
}

impl From<EventTx<RcvTx>> for EventTx<DownloadTx> {
    fn from(event: EventTx<RcvTx>) -> Self {
        EventTx {
            tx: event.tx,
            tx_type: DownloadTx,
        }
    }
}

impl From<EventTx<DownloadTx>> for EventTx<NewTx> {
    fn from(event: EventTx<DownloadTx>) -> Self {
        EventTx {
            tx: event.tx,
            tx_type: NewTx,
        }
    }
}

impl From<EventTx<DownloadTx>> for Option<EventTx<PropagateTx>> {
    fn from(event: EventTx<DownloadTx>) -> Self {
        match event.tx.state {
            TxReceive::P2P => None,
            TxReceive::RPC => Some(EventTx {
                tx: event.tx,
                tx_type: PropagateTx,
            }),
            TxReceive::TXRESULT => Some(EventTx {
                tx: event.tx,
                tx_type: PropagateTx,
            }),
        }
    }
}

//Processing of event that arrive: SourceTxType.
impl EventTx<RcvTx> {
    pub async fn process_event(
        self,
        acl_whitelist: &impl AclWhitelist,
    ) -> Result<EventTx<DownloadTx>, EventProcessError> {
        match self.validate_tx(acl_whitelist).await {
            Ok(()) => Ok(self.into()),
            Err(err) => Err(EventProcessError::ValidateError(format!(
                "Tx validation fail:{err}"
            ))),
        }
    }

    //Tx validation process.
    async fn validate_tx(&self, acl_whitelist: &impl AclWhitelist) -> Result<(), TransactionError> {
        self.tx.validate()?;

        // Secondly verify that author is whitelisted.
        if !acl_whitelist.contains(&self.tx.author).await? {
            return Err(TransactionError::Validation(
                "Tx permission denied signer not authorized".to_string(),
            ));
        }

        Ok(())
    }
}

//Download Tx processing
impl EventTx<DownloadTx> {
    pub async fn process_event(
        self,
        local_directory_path: &PathBuf,
        http_peer_list: Vec<(SocketAddr, Option<u16>)>,
    ) -> Result<(EventTx<NewTx>, Option<EventTx<PropagateTx>>), EventProcessError> {
        let tx_hash = self.tx.hash;
        let http_client = reqwest::Client::new();
        let asset_file_list = self.tx.get_asset_list(tx_hash).map_err(|err| {
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
            .map(|res| res)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|err| {
                EventProcessError::DownloadAssetError(format!("Exwecution error:{err}"))
            })?;
        let newtx: EventTx<NewTx> = self.clone().into();
        let propagate: Option<EventTx<PropagateTx>> = self.into();
        Ok((newtx, propagate))
    }
}

//Propagate Tx processing
impl EventTx<PropagateTx> {
    pub async fn process_event(
        self,
        p2p_sender: &UnboundedSender<Transaction<TxValdiated>>,
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
            state: TxValdiated,
        };
        tracing::info!("Tx validation propagate tx:{}", tx.hash.to_string());
        p2p_sender.send(tx).map_err(|err| err.into())
    }
}

impl EventTx<NewTx> {
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
            state: TxValdiated,
        };
        tracing::info!("Tx validation save tx:{}", tx.hash.to_string());
        mempool
            .add(tx)
            .map_err(|err| EventProcessError::SaveTxError(format!("{err}")))
            .await
    }
}
