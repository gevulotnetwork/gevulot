use async_trait::async_trait;
use eyre::Result;
use libsecp256k1::PublicKey;
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

#[async_trait]
pub trait AclWhitelist: Send + Sync {
    async fn contains(&self, key: &PublicKey) -> Result<bool>;
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
    acl_whitelist: Arc<dyn AclWhitelist>,
    // TODO: This should be refactored to PubSub channel abstraction later on.
    tx_chan: Option<Arc<dyn networking::p2p::TxChannel>>,
    deque: VecDeque<Transaction>,
}

impl Mempool {
    pub async fn new(
        storage: Arc<dyn Storage>,
        acl_whitelist: Arc<dyn AclWhitelist>,
        tx_chan: Option<Arc<dyn networking::p2p::TxChannel>>,
    ) -> Result<Self> {
        let mut deque = VecDeque::new();
        storage.fill_deque(&mut deque).await?;

        Ok(Self {
            storage,
            acl_whitelist,
            tx_chan,
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
        // First validate transaction.
        tx.validate()?;

        // Secondly verify that author is whitelisted.
        if !self.acl_whitelist.contains(&tx.author).await? {
            return Err(MempoolError::PermissionDenied.into());
        }

        let mut tx = tx;
        self.storage.set(&tx).await?;

        // Broadcast new transaction to P2P network if it's configured.
        if !tx.propagated {
            if let Some(ref tx_chan) = self.tx_chan {
                if tx_chan.send_tx(&tx).await.is_ok() {
                    tx.propagated = true;
                    self.storage.set(&tx).await?;
                } else {
                    // TODO: Implement retry?
                }
            }
        }

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
