use crate::txvalidation::ValidatedTxReceiver;
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
    //deque: VecDeque<Transaction<Validated>>,
    exec_sender: tokio::sync::mpsc::Sender<Transaction<Validated>>,
}

impl Mempool {
    pub async fn new(
        storage: Arc<dyn Storage>,
        exec_sender: tokio::sync::mpsc::Sender<Transaction<Validated>>,
    ) -> Result<Self> {
        let mut unexec_tx_deque = VecDeque::new();
        storage.fill_deque(&mut unexec_tx_deque).await?;
        for tx in unexec_tx_deque {
            tracing::trace!("Mempool init Rexecuting tx:{}", tx.hash);
            if let Err(err) = exec_sender.send(tx).await {
                tracing::error!("Error during sending unexecuted Tx during start :{err}");
            }
        }

        Ok(Self {
            storage,
            exec_sender,
        })
    }

    pub async fn add(&mut self, tx: Transaction<Validated>) -> Result<()> {
        self.storage.set(&tx).await?;
        //        self.deque.push_back(tx);
        self.exec_sender.send(tx).await?;
        Ok(())
    }
}

#[async_trait::async_trait]
impl ValidatedTxReceiver for Mempool {
    async fn send_new_tx(&mut self, tx: Transaction<Validated>) -> eyre::Result<()> {
        self.add(tx).await
    }
}
