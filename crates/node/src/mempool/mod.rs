use async_trait::async_trait;
use eyre::Result;
use std::collections::VecDeque;
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::RwLock;

use crate::{
    networking,
    types::{Hash, Transaction},
};

#[async_trait]
pub trait Storage: Send + Sync {
    async fn get(&self, hash: &Hash) -> Result<Option<Transaction>>;
    async fn set(&self, tx: &Transaction) -> Result<()>;
    async fn fill_deque(&self, deque: &mut VecDeque<Transaction>) -> Result<()>;
}

#[allow(clippy::enum_variant_names)]
#[derive(Error, Debug)]
pub enum MempoolError {
    #[error("permission denied")]
    PermissionDenied,
}

#[derive(Clone)]
pub struct Mempool {
    storage: Arc<dyn Storage>,
    //    acl_whitelist: Arc<dyn AclWhitelist>,
    // TODO: This should be refactored to PubSub channel abstraction later on.
    //    tx_chan: Option<Arc<dyn networking::p2p::TxChannel>>,
    deque: VecDeque<Transaction>,
}

impl Mempool {
    pub async fn new(
        storage: Arc<dyn Storage>,
        //        acl_whitelist: Arc<dyn AclWhitelist>,
        //        tx_chan: Option<Arc<dyn networking::p2p::TxChannel>>,
    ) -> Result<Self> {
        let mut deque = VecDeque::new();
        storage.fill_deque(&mut deque).await?;

        Ok(Self {
            storage,
            // acl_whitelist,
            // tx_chan,
            deque,
        })
    }

    pub fn next(&mut self) -> Option<Transaction> {
        // TODO(tuommaki): Should storage reflect the POP in state?
        self.deque.pop_front()
    }

    pub fn peek(&self) -> Option<&Transaction> {
        self.deque.front()
    }

    pub async fn add(&mut self, tx: Transaction) -> Result<()> {
        self.storage.set(&tx).await?;
        self.deque.push_back(tx);
        Ok(())
    }

    pub fn size(&self) -> usize {
        self.deque.len()
    }
}

pub struct P2PTxHandler(Arc<RwLock<Mempool>>);

#[async_trait::async_trait]
impl networking::p2p::TxHandler for P2PTxHandler {
    async fn recv_tx(&self, tx: Transaction) -> Result<()> {
        self.0.write().await.add(tx).await
    }
}
