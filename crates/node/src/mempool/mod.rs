use crate::txvalidation::ValidatedTxreceiver;
use crate::types::{transaction::Validated, Hash, Transaction};
use async_trait::async_trait;
use eyre::Result;
use std::collections::VecDeque;
use std::sync::Arc;
use thiserror::Error;

#[async_trait]
pub trait Storage: Send + Sync {
    async fn get(&self, hash: &Hash) -> Result<Option<Transaction<Validated>>>;
    async fn set(&self, tx: &Transaction<Validated>) -> Result<()>;
    async fn fill_deque(&self, deque: &mut VecDeque<Transaction<Validated>>) -> Result<()>;
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
    deque: VecDeque<Transaction<Validated>>,
}

impl Mempool {
    pub async fn new(storage: Arc<dyn Storage>) -> Result<Self> {
        let mut deque = VecDeque::new();
        storage.fill_deque(&mut deque).await?;

        Ok(Self { storage, deque })
    }

    pub fn next(&mut self) -> Option<Transaction<Validated>> {
        // TODO(tuommaki): Should storage reflect the POP in state?
        self.deque.pop_front()
    }

    pub fn peek(&self) -> Option<&Transaction<Validated>> {
        self.deque.front()
    }

    pub async fn add(&mut self, tx: Transaction<Validated>) -> Result<()> {
        self.storage.set(&tx).await?;
        self.deque.push_back(tx);
        Ok(())
    }

    pub fn size(&self) -> usize {
        self.deque.len()
    }
}

#[async_trait]
impl ValidatedTxreceiver for Mempool {
    async fn send_new_tx(&mut self, tx: Transaction<Validated>) -> eyre::Result<()> {
        self.add(tx).await
    }
}
