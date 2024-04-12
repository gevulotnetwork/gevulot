use crate::mempool::ValidateStorage;
use crate::types::Hash;
use crate::types::{
    transaction::{Execute, Received, Validated},
    Program, Transaction,
};
use futures_util::TryFutureExt;
use lru::LruCache;
use std::collections::HashMap;
use std::fmt::Debug;
use std::future::Future;
use std::marker::PhantomData;
use std::time::Duration;
use std::time::Instant;
use thiserror::Error;

const MAX_CACHED_TX_FOR_VERIFICATION: usize = 50;
const MAX_WAITING_TX_FOR_VERIFICATION: usize = 100;
const MAX_WAITING_TIME_IN_MS: u64 = 3600 * 1000; // One hour.

#[allow(clippy::enum_variant_names)]
#[derive(Error, Clone, Debug)]
pub enum PreexecError {
    #[error("Error during Tx processing: {0}")]
    PreProcessError(String),
    #[error("Error during Tx saving: {0}")]
    StorageError(String),
}

#[derive(Debug, Clone)]
pub struct PreExecTx;

#[derive(Debug, Clone)]
pub struct ExecTx;

//Event processing depends on the marker type.
#[derive(Debug, Clone)]
pub struct TxPreExecEvent<T: Debug> {
    pub tx: Transaction<Validated>,
    pub tx_type: T,
}

impl From<Transaction<Received>> for TxPreExecEvent<PreExecTx> {
    fn from(tx: Transaction<Received>) -> Self {
        let tx = Transaction {
            author: tx.author,
            hash: tx.hash,
            payload: tx.payload,
            nonce: tx.nonce,
            signature: tx.signature,
            //TODO should be updated after the p2p send with a notification
            propagated: true,
            executed: tx.executed,
            state: Validated,
        };

        TxPreExecEvent {
            tx,
            tx_type: PreExecTx,
        }
    }
}

impl From<TxPreExecEvent<PreExecTx>> for TxPreExecEvent<ExecTx> {
    fn from(event: TxPreExecEvent<PreExecTx>) -> Self {
        TxPreExecEvent {
            tx: event.tx,
            tx_type: ExecTx,
        }
    }
}

impl From<TxPreExecEvent<ExecTx>> for Transaction<Execute> {
    fn from(event: TxPreExecEvent<ExecTx>) -> Self {
        Transaction {
            author: event.tx.author,
            hash: event.tx.hash,
            payload: event.tx.payload,
            nonce: event.tx.nonce,
            signature: event.tx.signature,
            //TODO should be updated after the p2p send with a notification
            propagated: event.tx.propagated,
            executed: event.tx.executed,
            state: Execute,
        }
    }
}

// Run Tx depends on Deploy Tx program.
// Proof/Verify Tx depends on Run Tx to propagated.
// Do it in 2 step.
// Manage Run Tx and put to wait depending on progam.
// Manage Proof and Verify Tx to wait depending on Run Tx.
impl TxPreExecEvent<PreExecTx> {
    pub async fn validate_tx_dep(
        self,
        programid_cache: &mut TxCache<Program>,
        parent_cache: &mut TxCache<Transaction<Validated>>,
        storage: &impl ValidateStorage,
    ) -> Result<Vec<TxPreExecEvent<ExecTx>>, PreexecError> {
        // First validate program dep
        let prg_ok_txs = self.manage_program_dep(programid_cache, storage).await?;
        //validate parent dep
        let mut new_txs = vec![];
        for prog_ok_tx in prg_ok_txs {
            let mut ret = prog_ok_tx
                .manage_parent_dep(programid_cache, parent_cache, storage)
                .await?;
            new_txs.append(&mut ret);
        }

        // Transfort tx in PreExecTx
        let ret = new_txs.into_iter().map(|tx| tx.into()).collect();

        Ok(ret)
    }

    async fn wait_if_not_cached<'a, Kind, Fut>(
        self,
        cache: &mut TxCache<Kind>,
        cache_key: std::option::Option<&'a gevulot_node::types::Hash>,
        storage: &'a impl ValidateStorage,
        storage_contains: impl FnOnce(&'a Hash, &'a dyn ValidateStorage) -> Fut,
    ) -> Result<Option<Self>, PreexecError>
    where
        Fut: Future<Output = eyre::Result<bool>>,
    {
        if let Some(cache_key) = cache_key {
            if cache.is_tx_cached(cache_key)
                || storage_contains(cache_key, storage)
                    .await
                    .map_err(|err| PreexecError::StorageError(format!("{err}")))?
            {
                // Present return the Tx
                Ok(Some(self))
            } else {
                // Program is missing add to waiting list
                cache.add_new_waiting_tx(*cache_key, self);
                Ok(None)
            }
        } else {
            Ok(Some(self))
        }
    }

    // Validate if the Tx is a Run, its program has been installed.
    // If not, wait the Tx until the program deploy Tx arrives
    pub async fn manage_program_dep(
        self,
        programid_cache: &mut TxCache<Program>,
        storage: &impl ValidateStorage,
    ) -> Result<Vec<TxPreExecEvent<PreExecTx>>, PreexecError> {
        // Verify that Tx associated program are present.
        let run_tx_programs = self.tx.payload.get_run_programs_dep();
        fn query_db_for_program<'a>(
            cache_key: &'a Hash,
            storage: &'a dyn ValidateStorage,
        ) -> impl Future<Output = eyre::Result<bool>> + 'a {
            storage.contains_program(*cache_key)
        }

        // Test  all Tx progam id because Run tx can have program from different deploy Tx.
        let mut new_tx: Option<Self> = Some(self);
        for program_id in run_tx_programs {
            // Cache for only one dep.
            // If the tx depends on several deploy tx,
            // The Tx need to wait for all.
            // To avoid to cache for all dep,
            // released Run Tx are re validated with program id.
            new_tx = new_tx
                .unwrap()
                .wait_if_not_cached(
                    programid_cache,
                    Some(&program_id),
                    storage,
                    query_db_for_program,
                )
                .await?;
            if new_tx.is_none() {
                break;
            }
        }

        let new_txs = new_tx
            .map(|tx| {
                //if it's a deploy Tx free the Tx that are waiting.
                let mut ret_tx: Vec<_> = tx
                    .tx
                    .payload
                    .get_deploy_programs()
                    .into_iter()
                    .flat_map(|prg| {
                        //add deploy prg to the cache
                        programid_cache.add_cached_tx(prg);
                        programid_cache.remove_waiting_children_txs(&prg)
                    })
                    .collect();
                // Add the deploy Tx is any first to be processed the first.
                ret_tx.insert(0, tx);
                ret_tx
            })
            .unwrap_or_default();

        Ok(new_txs)
    }

    pub async fn manage_parent_dep(
        self,
        programid_cache: &mut TxCache<Program>,
        parent_cache: &mut TxCache<Transaction<Validated>>,
        storage: &impl ValidateStorage,
    ) -> Result<Vec<TxPreExecEvent<PreExecTx>>, PreexecError> {
        // Verify Tx'x parent is present or not.
        fn query_db_for_tx<'a>(
            cache_key: &'a Hash,
            storage: &'a dyn ValidateStorage,
        ) -> impl Future<Output = eyre::Result<bool>> + 'a {
            storage
                .get_tx(cache_key)
                .and_then(|res| async move { Ok(res.is_some()) })
        }
        let parent = self.tx.payload.get_parent_tx().cloned();
        let new_tx = self
            .wait_if_not_cached(parent_cache, parent.as_ref(), storage, query_db_for_tx)
            .await?;

        // Remove child Tx if any from waiting list.
        let tmp_new_txs = new_tx
            .map(|tx| {
                let mut ret = parent_cache.remove_waiting_children_txs(&tx.tx.hash);
                // Add the parent Tx first to be processed the first.
                ret.insert(0, tx);
                ret
            })
            .unwrap_or(vec![]);

        //Re validated Run tx for program dep.
        let mut new_txs = vec![];
        for new_tx in tmp_new_txs {
            if new_tx.tx.payload.is_run_payload() {
                let mut valid_txs = new_tx.manage_program_dep(programid_cache, storage).await?;
                new_txs.append(&mut valid_txs);
            } else {
                new_txs.push(new_tx);
            }
        }

        // Add new tx to the cache.
        for new_tx in &new_txs {
            parent_cache.add_cached_tx(new_tx.tx.hash);
        }
        Ok(new_txs)
    }
}

// Use to cache New tx and store waiting Tx that have missing parent.
pub struct TxCache<Kind> {
    // List of Tx waiting for parent.let waiting_txs =
    waiting_tx: HashMap<Hash, (Vec<TxPreExecEvent<PreExecTx>>, Instant)>,
    // Cache of the last saved Tx in the DB. To avoid to query the db for Tx.
    cached_tx_for_verification: LruCache<Hash, PreExecTx>,
    // Number of Waiting Tx that trigger the Tx eviction process.
    max_waiting_tx: usize,
    // Max time a Tx can wait in millisecond.
    max_waiting_time: Duration,
    //marker to avoid to mix cache use
    _marker: PhantomData<Kind>,
}

impl<Kind> TxCache<Kind> {
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
            _marker: PhantomData,
        }
    }

    pub fn is_tx_cached(&self, hash: &Hash) -> bool {
        self.cached_tx_for_verification.contains(hash)
    }

    pub fn add_cached_tx(&mut self, hash: Hash) {
        self.cached_tx_for_verification.put(hash, PreExecTx);
    }

    pub fn add_new_waiting_tx(&mut self, parent: Hash, tx: TxPreExecEvent<PreExecTx>) {
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

    pub fn remove_waiting_children_txs(&mut self, parent: &Hash) -> Vec<TxPreExecEvent<PreExecTx>> {
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

impl<Kind> Default for TxCache<Kind> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::mempool::Created;
    use crate::types::transaction::Payload;
    use crate::types::transaction::ProgramMetadata;
    use crate::types::transaction::{Workflow, WorkflowStep};
    use eyre::Result;
    use libsecp256k1::SecretKey;
    use rand::{rngs::StdRng, SeedableRng};
    use std::collections::HashSet;
    use tokio::sync::Mutex;

    struct TestDb {
        tx_db: Mutex<HashMap<Hash, Transaction<Validated>>>,
        program_db: Mutex<HashSet<Hash>>,
    }

    impl TestDb {
        async fn set_tx(&self, tx: Transaction<Validated>) {
            self.tx_db.lock().await.insert(tx.hash, tx);
        }
    }

    #[async_trait::async_trait]
    impl ValidateStorage for TestDb {
        async fn get_tx(&self, hash: &Hash) -> Result<Option<Transaction<Validated>>> {
            Ok(self.tx_db.lock().await.get(hash).cloned())
        }
        async fn contains_program(&self, hash: Hash) -> eyre::Result<bool> {
            Ok(self.program_db.lock().await.contains(hash.as_ref()))
        }
    }

    fn new_empty_tx_event() -> TxPreExecEvent<PreExecTx> {
        new_tx_event(Payload::Empty)
    }

    fn new_proof_tx_event(parent: Hash) -> TxPreExecEvent<PreExecTx> {
        let payload = Payload::Proof {
            parent,
            prover: Hash::default(),
            proof: vec![],
            files: vec![],
        };

        new_tx_event(payload)
    }

    fn new_deploy_tx_event(seed: u8) -> TxPreExecEvent<PreExecTx> {
        let prover_program = ProgramMetadata {
            hash: Hash::new([seed; 32]),
            ..Default::default()
        };
        let verifier_program = ProgramMetadata {
            hash: Hash::new([seed + 1; 32]),
            ..Default::default()
        };
        let payload = Payload::Deploy {
            name: "test".to_string(),
            prover: prover_program,
            verifier: verifier_program,
        };

        new_tx_event(payload)
    }

    fn new_run_tx_event(seed: u8) -> TxPreExecEvent<PreExecTx> {
        new_run_tx_event_for_programs(seed, seed + 1)
    }
    fn new_run_tx_event_for_programs(
        proover_seed: u8,
        verifier_seed: u8,
    ) -> TxPreExecEvent<PreExecTx> {
        let prover_program = WorkflowStep {
            program: Hash::new([proover_seed; 32]),
            args: vec![],
            inputs: vec![],
        };
        let verifier_program = WorkflowStep {
            program: Hash::new([verifier_seed; 32]),
            args: vec![],
            inputs: vec![],
        };
        let workflow = Workflow {
            steps: vec![prover_program, verifier_program],
        };
        let payload = Payload::Run { workflow };

        new_tx_event(payload)
    }

    fn new_tx_event(payload: Payload) -> TxPreExecEvent<PreExecTx> {
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
            state: Validated,
        };
        TxPreExecEvent {
            tx,
            tx_type: PreExecTx,
        }
    }

    #[tokio::test]
    async fn test_wait_tx_for_different_deploy_program() {
        let db = TestDb {
            tx_db: Mutex::new(HashMap::new()),
            program_db: Mutex::new(HashSet::new()),
        };
        // Set parameters to have Tx eviction
        let mut wait_progam_cache = TxCache::<Program>::build(4, 2, 10);
        let mut wait_tx_cache = TxCache::<Transaction<Validated>>::build(2, 2, 10);

        // Create the parent Txs that will only be processed at the end.
        // Create 2 parents to have 2 waiting child Tx
        let deploy1_tx_event = new_deploy_tx_event(1);
        let deploy2_tx_event = new_deploy_tx_event(3);
        let run1_tx_event = new_run_tx_event_for_programs(1, 4); //(proof Tx1, verif Tx2)

        // Valide Run Tx. Put in Wait cache
        let res = run1_tx_event
            .validate_tx_dep(&mut wait_progam_cache, &mut wait_tx_cache, &db)
            .await;
        assert!(res.is_ok());
        // Run Tx is waiting
        assert_eq!(wait_progam_cache.waiting_tx.len(), 1);
        assert_eq!(0, res.unwrap().len());
        // No programs in cache.
        assert_eq!(wait_progam_cache.cached_tx_for_verification.len(), 0);

        // Deploy1 Tx, only return the deploy Tx. Wait for deploy2 program.
        let res = deploy1_tx_event
            .validate_tx_dep(&mut wait_progam_cache, &mut wait_tx_cache, &db)
            .await;
        assert!(res.is_ok());
        assert_eq!(wait_progam_cache.waiting_tx.len(), 1);
        assert_eq!(1, res.unwrap().len());
        // 2 programs cached
        assert_eq!(wait_progam_cache.cached_tx_for_verification.len(), 2);

        // Deploy2 Tx, return 2 tx, deploy2 + Run.
        let res = deploy2_tx_event
            .validate_tx_dep(&mut wait_progam_cache, &mut wait_tx_cache, &db)
            .await;
        assert!(res.is_ok());
        //remove from cache
        assert_eq!(wait_progam_cache.waiting_tx.len(), 0);
        assert_eq!(2, res.unwrap().len());
        // 4 programs cached
        assert_eq!(wait_progam_cache.cached_tx_for_verification.len(), 4);
    }
    #[tokio::test]
    async fn test_wait_tx_for_program() {
        let db = TestDb {
            tx_db: Mutex::new(HashMap::new()),
            program_db: Mutex::new(HashSet::new()),
        };
        // Set parameters to have Tx eviction
        let mut wait_progam_cache = TxCache::<Program>::build(4, 2, 10);

        // Create the parent Txs that will only be processed at the end.
        // Create 2 parents to have 2 waiting child Tx
        let deploy1_tx_event = new_deploy_tx_event(1);
        let run1_tx_event = new_run_tx_event(1);

        // First do normal process
        // Deploy then Run.
        let res = deploy1_tx_event
            .manage_program_dep(&mut wait_progam_cache, &db)
            .await;
        assert!(res.is_ok());
        assert_eq!(wait_progam_cache.waiting_tx.len(), 0);
        assert_eq!(1, res.unwrap().len());
        // 2 programs cached
        assert_eq!(wait_progam_cache.cached_tx_for_verification.len(), 2);

        let res = run1_tx_event
            .manage_program_dep(&mut wait_progam_cache, &db)
            .await;
        assert!(res.is_ok());
        assert_eq!(wait_progam_cache.waiting_tx.len(), 0);
        assert_eq!(1, res.unwrap().len());
        // 2 same programs cached. No changes.
        assert_eq!(wait_progam_cache.cached_tx_for_verification.len(), 2);

        // Test Run Tx waiting for deploy Tx and proof tx not waiting.
        let deploy2_tx_event = new_deploy_tx_event(3);
        let run2_tx_event = new_run_tx_event(3);
        let proof2_tx_event = new_proof_tx_event(run2_tx_event.tx.hash);
        let res = run2_tx_event
            .manage_program_dep(&mut wait_progam_cache, &db)
            .await;
        assert!(res.is_ok());
        //Run Tx is waiting
        assert_eq!(wait_progam_cache.waiting_tx.len(), 1);
        assert_eq!(0, res.unwrap().len());
        // No new cache done.
        assert_eq!(wait_progam_cache.cached_tx_for_verification.len(), 2);

        let res = proof2_tx_event
            .manage_program_dep(&mut wait_progam_cache, &db)
            .await;
        assert!(res.is_ok());
        assert_eq!(wait_progam_cache.waiting_tx.len(), 1);
        // Not managed by program. ProofTx return. Will wait with parent detection.
        assert_eq!(1, res.unwrap().len());
        // No new  cache done.
        assert_eq!(wait_progam_cache.cached_tx_for_verification.len(), 2);

        let deploy2_tx_hash = deploy2_tx_event.tx.hash;
        let res = deploy2_tx_event
            .manage_program_dep(&mut wait_progam_cache, &db)
            .await;
        assert!(res.is_ok());
        // Run Tx remove from cache
        assert_eq!(wait_progam_cache.waiting_tx.len(), 0);
        // Run Tx + Deploy
        let res = res.unwrap();
        assert_eq!(2, res.len());
        //deploy tx first.
        assert_eq!(res[0].tx.hash, deploy2_tx_hash);
        // +2 programs cached
        assert_eq!(wait_progam_cache.cached_tx_for_verification.len(), 4);

        //test  case miss and db query and get from db.
        db.program_db.lock().await.insert(Hash::new([5; 32]));
        db.program_db.lock().await.insert(Hash::new([6; 32]));
        let deploy_tx_event = new_deploy_tx_event(5);
        let run_tx_event = new_run_tx_event(5);
        let res = run_tx_event
            .manage_program_dep(&mut wait_progam_cache, &db)
            .await;
        assert!(res.is_ok());
        //Run Tx is waiting
        assert_eq!(wait_progam_cache.waiting_tx.len(), 0);
        assert_eq!(1, res.unwrap().len());
        // No new cache done.
        assert_eq!(wait_progam_cache.cached_tx_for_verification.len(), 4);
    }

    #[tokio::test]
    async fn test_evict_wait_tx() {
        let db = TestDb {
            tx_db: Mutex::new(HashMap::new()),
            program_db: Mutex::new(HashSet::new()),
        };
        // Set parameters to have Tx eviction
        let mut wait_progam_cache = TxCache::<Program>::build(2, 2, 10);
        let mut wait_tx_cache = TxCache::<Transaction<Validated>>::build(2, 2, 10);

        // Create the parent Txs that will only be processed at the end.
        // Create 2 parents to have 2 waiting child Tx
        let parent1_tx_event = new_empty_tx_event();
        let parent1_hash = parent1_tx_event.tx.hash;
        let parent2_tx_event = new_empty_tx_event();
        let parent2_hash = parent2_tx_event.tx.hash;

        // New Tx that will wait.
        let tx_event = new_proof_tx_event(parent1_hash);
        let res = tx_event
            .validate_tx_dep(&mut wait_progam_cache, &mut wait_tx_cache, &db)
            .await;
        assert!(res.is_ok());
        assert_eq!(wait_tx_cache.waiting_tx.len(), 1);

        // New Tx that will wait.
        let tx_event = new_proof_tx_event(parent2_hash);
        let res = tx_event
            .validate_tx_dep(&mut wait_progam_cache, &mut wait_tx_cache, &db)
            .await;
        assert!(res.is_ok());
        assert_eq!(wait_tx_cache.waiting_tx.len(), 2);

        tokio::time::sleep(Duration::from_millis(100)).await;

        // Add new Tx to evict the old one.
        let tx_event = new_proof_tx_event(parent1_hash);
        let tx_hash = tx_event.tx.hash;
        let res = tx_event
            .validate_tx_dep(&mut wait_progam_cache, &mut wait_tx_cache, &db)
            .await;
        assert!(res.is_ok());
        // Evicted but new tx added => len=1.
        assert_eq!(wait_tx_cache.waiting_tx.len(), 1);

        //Process the parent Tx. Do child Tx return because they have been evicted.
        let res = parent1_tx_event
            .validate_tx_dep(&mut wait_progam_cache, &mut wait_tx_cache, &db)
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
        let db = TestDb {
            tx_db: Mutex::new(HashMap::new()),
            program_db: Mutex::new(HashSet::new()),
        };
        // Set parameters to avoid wait tx eviction.
        let mut wait_progam_cache = TxCache::<Program>::build(2, 2, 10);
        let mut wait_tx_cache = TxCache::<Transaction<Validated>>::build(2, 2, 10);

        // Test a new tx without parent. No wait and added to cache.
        let tx_event1 = new_empty_tx_event();
        let tx1_hash = tx_event1.tx.hash;
        let tx1 = tx_event1.tx.clone();
        let res = tx_event1
            .validate_tx_dep(&mut wait_progam_cache, &mut wait_tx_cache, &db)
            .await;
        assert!(res.is_ok());
        // Save the Tx in db to test cache miss.
        let _ = db.set_tx(tx1).await;
        // Not in wait cache because no parent.
        assert_eq!(wait_tx_cache.waiting_tx.len(), 0);
        assert!(wait_tx_cache.is_tx_cached(&tx1_hash));

        // Test a new tx with a present parent. No wait and added to cache.
        let tx2_event = new_proof_tx_event(tx1_hash);
        let tx2_hash = tx2_event.tx.hash;
        let tx2 = tx2_event.tx.clone();
        let res = tx2_event
            .validate_tx_dep(&mut wait_progam_cache, &mut wait_tx_cache, &db)
            .await;
        assert!(res.is_ok());
        // Save the Tx in db to test cache miss.
        let _ = db.set_tx(tx2).await;
        // Not cached because no parent.
        assert_eq!(wait_tx_cache.waiting_tx.len(), 0);
        assert!(wait_tx_cache.is_tx_cached(&tx2_hash));

        // Test a new Tx with a missing parent. Waiting / not cached.
        let parent_tx_event = new_empty_tx_event();
        let parent_hash = parent_tx_event.tx.hash;
        let parent_tx = parent_tx_event.tx.clone();
        let tx_event3 = new_proof_tx_event(parent_tx_event.tx.hash);
        let tx3_hash = tx_event3.tx.hash;
        let res = tx_event3
            .validate_tx_dep(&mut wait_progam_cache, &mut wait_tx_cache, &db)
            .await;
        assert!(res.is_ok());
        assert_eq!(wait_tx_cache.waiting_tx.len(), 1);
        assert!(!wait_tx_cache.is_tx_cached(&tx3_hash));

        // Test process parent Tx: Tx3 removed from waiting, Tx3 and parent added to cached. Return Tx3 and parent.
        let res = parent_tx_event
            .validate_tx_dep(&mut wait_progam_cache, &mut wait_tx_cache, &db)
            .await;
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
        let res = tx4_event
            .validate_tx_dep(&mut wait_progam_cache, &mut wait_tx_cache, &db)
            .await;
        assert!(res.is_ok());
        // Not cached because parent in db
        assert_eq!(wait_tx_cache.waiting_tx.len(), 0);
        assert!(wait_tx_cache.is_tx_cached(&tx4_hash));
    }
}
