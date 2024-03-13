use super::ValidatedTxReceiver;
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
use std::time::Duration;
use std::time::Instant;
use tokio::sync::mpsc::UnboundedSender;

const MAX_CACHED_TX_FOR_VERIFICATION: usize = 50;
const MAX_WAITING_TX_FOR_VERIFICATION: usize = 100;
const MAX_WAITING_TIME_IN_MS: u64 = 3600 * 1000; // One hour.

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
        cache: &mut TxCache,
        storage: &impl Storage,
    ) -> Result<Vec<TxEvent<NewTx>>, EventProcessError> {
        // Verify Tx'x parent is present or not.
        let new_tx = if let Some(parent) = self.tx.payload.get_parent_tx() {
            if cache.is_tx_cached(parent)
                || storage
                    .get(parent)
                    .await
                    .map_err(|err| EventProcessError::StorageError(format!("{err}")))?
                    .is_some()
            {
                // Present return the Tx
                Some(self)
            } else {
                // Parent is missing add to waiting list
                cache.add_new_waiting_tx(*parent, self);
                None
            }
        } else {
            // No parent always new Tx.
            Some(self)
        };

        //remove child Tx if any from waiting list.
        let new_txs = new_tx
            .map(|tx| {
                let mut ret = cache.remove_waiting_children_txs(&tx.tx.hash);
                // Add the parent Tx first to be processed the first.
                ret.insert(0, tx);
                ret
            })
            .unwrap_or(vec![]);

        // Add new tx to the cache.
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

// Use to cache New tx and store waiting Tx that have missing parent.
pub struct TxCache {
    // List of Tx waiting for parent.let waiting_txs =
    waiting_tx: HashMap<Hash, (Vec<TxEvent<WaitTx>>, Instant)>,
    // Cache of the last saved Tx in the DB. To avoid to query the db for Tx.
    cached_tx_for_verification: LruCache<Hash, WaitTx>,
    // Number of Waiting Tx that trigger the Tx eviction process.
    max_waiting_tx: usize,
    // Max time a Tx can wait in millisecond.
    max_waiting_time: Duration,
}

impl TxCache {
    pub fn new() -> Self {
        Self::build(
            MAX_CACHED_TX_FOR_VERIFICATION,
            MAX_WAITING_TX_FOR_VERIFICATION,
            MAX_WAITING_TIME_IN_MS,
        )
    }
    pub fn build(cache_size: usize, max_waiting_tx: usize, max_waiting_time: u64) -> Self {
        let cached_tx_for_verification =
            LruCache::new(std::num::NonZeroUsize::new(cache_size).unwrap());
        TxCache {
            waiting_tx: HashMap::new(),
            cached_tx_for_verification,
            max_waiting_tx,
            max_waiting_time: Duration::from_millis(max_waiting_time),
        }
    }

    pub fn is_tx_cached(&self, hash: &Hash) -> bool {
        self.cached_tx_for_verification.contains(hash)
    }

    pub fn add_cached_tx(&mut self, hash: Hash) {
        self.cached_tx_for_verification.put(hash, WaitTx);
    }

    pub fn add_new_waiting_tx(&mut self, parent: Hash, tx: TxEvent<WaitTx>) {
        // Try to evict when the max waiting Tx is reach.
        if self.waiting_tx.len() >= self.max_waiting_tx {
            self.evict_old_waiting_tx();
        }
        let (waiting_txs, _) = self
            .waiting_tx
            .entry(parent)
            .or_insert((vec![], Instant::now()));
        waiting_txs.push(tx);
    }

    pub fn remove_waiting_children_txs(&mut self, parent: &Hash) -> Vec<TxEvent<WaitTx>> {
        self.waiting_tx
            .remove(parent)
            .map(|(txs, _)| txs)
            .unwrap_or_default()
    }

    fn evict_old_waiting_tx(&mut self) {
        let now = Instant::now();
        let to_remove_hash: Vec<_> = self
            .waiting_tx
            .iter()
            .filter_map(|(hash, (_, ts))| {
                (now.duration_since(*ts) > self.max_waiting_time).then_some(*hash)
            })
            .collect();
        if !to_remove_hash.is_empty() {
            // Warn if some Tx are evicted because it shouldn't.
            tracing::warn!("Tx validation, Evict some Tx from waiting for parent tx list.");
            for hash in to_remove_hash {
                tracing::warn!("Tx validation, Evict Tx:{hash}.");
                self.waiting_tx.remove(&hash);
            }
        }
    }
}

impl Default for TxCache {
    fn default() -> Self {
        Self::new()
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

    fn new_empty_tx_event() -> TxEvent<WaitTx> {
        new_tx_event(Payload::Empty)
    }

    fn new_proof_tx_event(parent: Hash) -> TxEvent<WaitTx> {
        let payload = Payload::Proof {
            parent,
            prover: Hash::default(),
            proof: vec![],
            files: vec![],
        };

        new_tx_event(payload)
    }

    fn new_tx_event(payload: Payload) -> TxEvent<WaitTx> {
        let rng = &mut StdRng::from_entropy();

        let tx = Transaction::<Created>::new(payload, &SecretKey::random(rng));

        let tx = Transaction {
            author: tx.author,
            hash: tx.hash,
            payload: tx.payload,
            nonce: tx.nonce,
            signature: tx.signature,
            propagated: tx.executed,
            executed: tx.executed,
            state: Received::P2P,
        };
        TxEvent {
            tx,
            tx_type: WaitTx,
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
    async fn test_evict_wait_tx() {
        let db = TestDb(Mutex::new(HashMap::new()));
        // Set parameters to have Tx eviction
        let mut wait_tx_cache = TxCache::build(2, 2, 10);

        // Create the parent Txs that will only be processed at the end.
        // Create 2 parents to have 2 waiting child Tx
        let parent1_tx_event = new_empty_tx_event();
        let parent1_hash = parent1_tx_event.tx.hash;
        let parent2_tx_event = new_empty_tx_event();
        let parent2_hash = parent2_tx_event.tx.hash;

        // New Tx that will wait.
        let tx_event = new_proof_tx_event(parent1_hash);
        let res = tx_event.process_event(&mut wait_tx_cache, &db).await;
        assert!(res.is_ok());
        assert_eq!(wait_tx_cache.waiting_tx.len(), 1);

        // New Tx that will wait.
        let tx_event = new_proof_tx_event(parent2_hash);
        let res = tx_event.process_event(&mut wait_tx_cache, &db).await;
        assert!(res.is_ok());
        assert_eq!(wait_tx_cache.waiting_tx.len(), 2);

        tokio::time::sleep(Duration::from_millis(100)).await;

        // Add new Tx to evict the old one.
        let tx_event = new_proof_tx_event(parent1_hash);
        let tx_hash = tx_event.tx.hash;
        let res = tx_event.process_event(&mut wait_tx_cache, &db).await;
        assert!(res.is_ok());
        // Evicted but new tx added => len=1.
        assert_eq!(wait_tx_cache.waiting_tx.len(), 1);

        //Process the parent Tx. Do child Tx return because they have been evicted.
        let res = parent1_tx_event
            .process_event(&mut wait_tx_cache, &db)
            .await;
        assert!(res.is_ok());
        let ret_txs = res.unwrap();
        // Return only the parent Tx. The other has been evicted during parent Tx add.
        assert_eq!(ret_txs.len(), 2);
        assert!(ret_txs[0].tx.hash == parent1_hash || ret_txs[1].tx.hash == parent1_hash);
        assert!(ret_txs[0].tx.hash == tx_hash || ret_txs[1].tx.hash == tx_hash);
    }

    #[tokio::test]
    async fn test_wait_tx_process_event() {
        let db = TestDb(Mutex::new(HashMap::new()));
        // Set parameters to avoid wait tx eviction.
        let mut wait_tx_cache = TxCache::build(2, 10, 1000);

        // Test a new tx without parent. No wait and added to cache.
        let tx_event1 = new_empty_tx_event();
        let tx1_hash = tx_event1.tx.hash;
        let tx1 = tx_event1.tx.clone();
        let res = tx_event1.process_event(&mut wait_tx_cache, &db).await;
        assert!(res.is_ok());
        // Save the Tx in db to test cache miss.
        let _ = db.set(&into_receive(tx1)).await;
        // Not in wait cache because no parent.
        assert_eq!(wait_tx_cache.waiting_tx.len(), 0);
        assert!(wait_tx_cache.is_tx_cached(&tx1_hash));

        // Test a new tx with a present parent. No wait and added to cache.
        let tx2_event = new_proof_tx_event(tx1_hash);
        let tx2_hash = tx2_event.tx.hash;
        let tx2 = tx2_event.tx.clone();
        let res = tx2_event.process_event(&mut wait_tx_cache, &db).await;
        assert!(res.is_ok());
        // Save the Tx in db to test cache miss.
        let _ = db.set(&into_receive(tx2)).await;
        // Not cached because no parent.
        assert_eq!(wait_tx_cache.waiting_tx.len(), 0);
        assert!(wait_tx_cache.is_tx_cached(&tx2_hash));

        // Test a new Tx with a missing parent. Waiting / not cached.
        let parent_tx_event = new_empty_tx_event();
        let parent_hash = parent_tx_event.tx.hash;
        let parent_tx = parent_tx_event.tx.clone();
        let tx_event3 = new_proof_tx_event(parent_tx_event.tx.hash);
        let tx3_hash = tx_event3.tx.hash;
        let res = tx_event3.process_event(&mut wait_tx_cache, &db).await;
        assert!(res.is_ok());
        assert_eq!(wait_tx_cache.waiting_tx.len(), 1);
        assert!(!wait_tx_cache.is_tx_cached(&tx3_hash));

        // Test process parent Tx: Tx3 removed from waiting, Tx3 and parent added to cached. Return Tx3 and parent.
        let res = parent_tx_event.process_event(&mut wait_tx_cache, &db).await;
        assert!(res.is_ok());
        assert_eq!(wait_tx_cache.waiting_tx.len(), 0);
        assert!(wait_tx_cache.is_tx_cached(&parent_hash));
        assert!(wait_tx_cache.is_tx_cached(&tx3_hash));
        let ret_events = res.unwrap();
        assert_eq!(ret_events.len(), 2);
        assert!(ret_events[0].tx.hash == parent_hash || ret_events[1].tx.hash == parent_hash);
        assert!(ret_events[0].tx.hash == tx3_hash || ret_events[1].tx.hash == tx3_hash);

        //Test a cache miss, get the parent from the DB. No wait + cached
        assert!(!wait_tx_cache.is_tx_cached(&tx1_hash));
        let tx4_event = new_proof_tx_event(tx1_hash); // tx1_hash not in the cache but in the DB
        let tx4_hash = tx4_event.tx.hash;
        let res = tx4_event.process_event(&mut wait_tx_cache, &db).await;
        assert!(res.is_ok());
        // Not cached because parent in db
        assert_eq!(wait_tx_cache.waiting_tx.len(), 0);
        assert!(wait_tx_cache.is_tx_cached(&tx4_hash));
    }
}
