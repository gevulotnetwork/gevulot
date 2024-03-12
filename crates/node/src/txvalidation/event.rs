use crate::mempool::Storage;
use crate::txvalidation::acl::AclWhitelist;
use crate::txvalidation::download_manager;
use crate::txvalidation::EventProcessError;
use crate::types::Hash;
use crate::types::{
    transaction::{Received, Validated},
    Transaction,
};
use futures::future::join_all;
use futures_util::TryFutureExt;
use lru::LruCache;
use std::collections::HashMap;
use std::fmt::Debug;
use std::net::SocketAddr;
use std::path::Path;
use tokio::sync::mpsc::UnboundedSender;

use super::ValidatedTxReceiver;

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
                let mut new_txs = cache.remove_waiting_children_txs(parent);
                new_txs.push(self);
                new_txs
            } else {
                //parent is missing add to waiting list
                cache.add_new_waiting_tx(*parent, self);
                vec![]
            }
        } else {
            //no parent always new Tx.
            vec![self]
        };

        let ret = new_txs
            .into_iter()
            .map(|tx| {
                cache.add_cached_tx(tx.tx.hash);
                tx.into()
            })
            .collect();

        Ok(ret)
    }
}

impl TxEvent<NewTx> {
    pub async fn process_event(
        self,
        newtx_receiver: &mut dyn ValidatedTxReceiver,
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
            "Tx validation save tx:{}  payload:{}",
            tx.hash.to_string(),
            tx.payload
        );
        newtx_receiver
            .send_new_tx(tx)
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
    pub fn new(cache_size: usize) -> Self {
        let cachedtx_for_verification =
            LruCache::new(std::num::NonZeroUsize::new(cache_size).unwrap());
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

    pub fn add_new_waiting_tx(&mut self, parent: Hash, tx: TxEvent<WaitTx>) {
        let waiting_txs = self.waiting_tx.entry(parent).or_insert(vec![]);
        waiting_txs.push(tx);
    }

    pub fn remove_waiting_children_txs(&mut self, parent: &Hash) -> Vec<TxEvent<WaitTx>> {
        self.waiting_tx.remove(parent).unwrap_or(vec![])
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::txvalidation::Created;
    use crate::types::transaction::Payload;
    use eyre::Result;
    use libsecp256k1::SecretKey;
    use rand::{rngs::StdRng, SeedableRng};
    use std::collections::VecDeque;
    use tokio::sync::Mutex;

    struct TestDb(Mutex<HashMap<Hash, Transaction<Validated>>>);

    #[async_trait::async_trait]
    impl Storage for TestDb {
        async fn get(&self, hash: &Hash) -> Result<Option<Transaction<Validated>>> {
            Ok(self.0.lock().await.get(hash).cloned())
        }
        async fn set(&self, tx: &Transaction<Validated>) -> Result<()> {
            self.0.lock().await.insert(tx.hash, tx.clone());
            Ok(())
        }
        async fn fill_deque(&self, deque: &mut VecDeque<Transaction<Validated>>) -> Result<()> {
            Ok(())
        }
    }

    fn new_empty_tx() -> Transaction<Received> {
        new_tx(Payload::Empty)
    }

    fn new_proof_tx(parent: Hash) -> Transaction<Received> {
        let payload = Payload::Proof {
            parent: parent,
            prover: Hash::default(),
            proof: vec![],
            files: vec![],
        };

        new_tx(payload)
    }

    fn new_tx(payload: Payload) -> Transaction<Received> {
        let rng = &mut StdRng::from_entropy();

        let tx = Transaction::<Created>::new(Payload::Empty, &SecretKey::random(rng));

        Transaction {
            author: tx.author,
            hash: tx.hash,
            payload: tx.payload,
            nonce: tx.nonce,
            signature: tx.signature,
            propagated: tx.executed,
            executed: tx.executed,
            state: Received::P2P,
        }
    }

    fn into_receive(tx: Transaction<Received>) -> Transaction<Validated> {
        Transaction {
            author: tx.author,
            hash: tx.hash,
            payload: tx.payload,
            nonce: tx.nonce,
            signature: tx.signature,
            propagated: tx.executed,
            executed: tx.executed,
            state: Validated,
        }
    }

    #[tokio::test]
    async fn test_waittx_process_event() {
        let db = TestDb(Mutex::new(HashMap::new()));
        let mut wait_tx_cache = TXCache::new(2);
        let new_tx1 = new_empty_tx();
        let tx1_hash = new_tx1.hash;
        let tx1_event = TxEvent {
            tx: new_tx1.clone(),
            tx_type: WaitTx,
        };
        // Test a new tx without parent. No wait and added to cache.
        let res = tx1_event.process_event(&mut wait_tx_cache, &db).await;
        // Save the Tx in db to test cache miss.
        let _ = db.set(&into_receive(new_tx1)).await;
        assert!(res.is_ok());
        // Not cached because no parent.
        assert_eq!(wait_tx_cache.waiting_tx.len(), 0);
        assert!(wait_tx_cache.is_tx_cached(&tx1_hash));

        let new_tx2 = new_proof_tx(tx1_hash);

        let tx2_hash = new_tx2.hash;
    }
}
