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

    persist_only: bool,
}

impl Mempool {
    // If `persist_only` flag is set as `true`, the mempool won't populate
    // `deque` with transactions. This can be used to disable transaction
    // execution, while keeping rest of the node functionality as-is.
    // Main use case for this is as a JSON-RPC API + Archive node.
    pub async fn new(storage: Arc<dyn Storage>, persist_only: bool) -> Result<Self> {
        let mut deque = VecDeque::new();

        if !persist_only {
            storage.fill_deque(&mut deque).await?;
        }

        Ok(Self {
            storage,
            deque,
            persist_only,
        })
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

        // If `persist_only` is set, don't provide transactions available
        // for consumption.
        if !self.persist_only {
            self.deque.push_back(tx);
        }

        Ok(())
    }

    pub fn size(&self) -> usize {
        self.deque.len()
    }
}
