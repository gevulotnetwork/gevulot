use crate::mempool::Storage;
use crate::txvalidation::acl::AclWhitelist;
use crate::txvalidation::download_manager;
use crate::txvalidation::EventProcessError;
use crate::txvalidation::MAX_CACHED_TX_FOR_VERIFICATION;
use crate::types::Hash;
use crate::types::{
    transaction::{Received, Validated},
    Transaction,
};
use crate::Mempool;
use futures::future::join_all;
use futures_util::TryFutureExt;
use lru::LruCache;
use std::collections::HashMap;
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
pub struct WaitTx;

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

impl From<TxEvent<DownloadTx>> for TxEvent<WaitTx> {
    fn from(event: TxEvent<DownloadTx>) -> Self {
        TxEvent {
            tx: event.tx,
            tx_type: WaitTx,
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

impl From<TxEvent<WaitTx>> for TxEvent<NewTx> {
    fn from(event: TxEvent<WaitTx>) -> Self {
        TxEvent {
            tx: event.tx,
            tx_type: NewTx,
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
    ) -> Result<(TxEvent<WaitTx>, Option<TxEvent<PropagateTx>>), EventProcessError> {
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
                EventProcessError::DownloadAssetError(format!("Execution error:{err}"))
            })?;
        let newtx: TxEvent<WaitTx> = self.clone().into();
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
        tracing::info!(
            "Tx validation propagate tx:{} payload:{}",
            tx.hash.to_string(),
            tx.payload
        );
        p2p_sender.send(tx).map_err(|err| Box::new(err).into())
    }
}

impl TxEvent<WaitTx> {
    pub async fn process_event(
        self,
        cache: &mut TXCache,
        storage: &impl Storage,
    ) -> Result<Vec<TxEvent<NewTx>>, EventProcessError> {
        // Verify Tx'x parent is already present.
        let new_txs = if let Some(parent) = self.tx.payload.get_parent_tx() {
            if cache.is_tx_cached(parent)
                || storage
                    .get(parent)
                    .await
                    .map_err(|err| EventProcessError::StorageError(format!("{err}")))?
                    .is_some()
            {
                let mut new_txs = cache.remove_children_txs(parent);
                new_txs.push(self);
                new_txs
            } else {
                //parent is missing add to waiting list
                cache.add_wait_tx(*parent, self);
                vec![]
            }
        } else {
            //no parent always new Tx.
            vec![self]
        };

        let ret = new_txs.into_iter().map(|tx| tx.into()).collect();

        Ok(ret)
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
        tracing::info!(
            "Tx validation save tx:{}  payload:{}",
            tx.hash.to_string(),
            tx.payload
        );
        mempool
            .add(tx)
            .map_err(|err| EventProcessError::SaveTxError(format!("{err}")))
            .await
    }
}

pub struct TXCache {
    // List of Tx waiting for parent.let waiting_txs =
    waiting_tx: HashMap<Hash, Vec<TxEvent<WaitTx>>>,
    // Cache of the last saved Tx in the DB. To avoid to query the db for Tx.
    cachedtx_for_verification: LruCache<Hash, WaitTx>,
}

impl TXCache {
    pub fn new() -> Self {
        let cachedtx_for_verification =
            LruCache::new(std::num::NonZeroUsize::new(MAX_CACHED_TX_FOR_VERIFICATION).unwrap());
        TXCache {
            waiting_tx: HashMap::new(),
            cachedtx_for_verification,
        }
    }

    pub fn is_tx_cached(&self, hash: &Hash) -> bool {
        self.cachedtx_for_verification.contains(hash)
    }

    pub fn add_cached_tx(&mut self, hash: Hash) {
        self.cachedtx_for_verification.put(hash, WaitTx);
    }

    pub fn add_wait_tx(&mut self, parent: Hash, tx: TxEvent<WaitTx>) {
        let waiting_txs = self.waiting_tx.entry(parent).or_insert(vec![]);
        waiting_txs.push(tx);
    }

    pub fn remove_children_txs(&mut self, parent: &Hash) -> Vec<TxEvent<WaitTx>> {
        self.waiting_tx.remove(parent).unwrap_or(vec![])
    }
}
