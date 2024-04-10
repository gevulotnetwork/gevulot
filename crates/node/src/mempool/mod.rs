use crate::types::{
    transaction::{Created, Received, Validated},
    Hash, Transaction,
};
use async_trait::async_trait;
use eyre::Result;
use futures_util::Stream;
use gevulot_node::acl;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::fmt::Debug;
use std::marker::PhantomData;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::oneshot;
use tokio::sync::RwLock;
use tokio::task::JoinHandle;

mod event;
mod txvalidation;

#[allow(clippy::enum_variant_names)]
#[derive(Error, Debug)]
pub enum MempoolError {
    #[error("Tx validation fail: {0}")]
    TxValidationError(String),
    #[error("permission denied")]
    PermissionDenied,
    #[error("Error during Tx processing")]
    EventProcess(#[from] event::EventProcessError),
    #[error("Fail to send the Tx on the channel: {0}")]
    SendChannelError(
        #[from]
        tokio::sync::mpsc::error::SendError<(Transaction<Received>, Option<CallbackSender>)>,
    ),
    #[error("Fail to rcv Tx from the channel: {0}")]
    RcvChannelError(#[from] tokio::sync::oneshot::error::RecvError),
}

pub type CallbackSender = oneshot::Sender<Result<(), MempoolError>>;

// Sending Tx interface.
// Some marker type to define the sender source.
pub struct RpcSender;
#[derive(Clone)]
pub struct P2pSender;
pub struct TxResultSender;

// `TxEventSender` holds the received transaction of a specific state together with an optional callback interface.
#[derive(Debug, Clone)]
pub struct TxEventSender<T> {
    sender: UnboundedSender<(Transaction<Received>, Option<CallbackSender>)>,
    _marker: PhantomData<T>,
}

//Manage send from the p2p source
impl TxEventSender<P2pSender> {
    pub fn build(sender: UnboundedSender<(Transaction<Received>, Option<CallbackSender>)>) -> Self {
        TxEventSender {
            sender,
            _marker: PhantomData,
        }
    }

    pub fn send_tx(&self, tx: Transaction<Created>) -> Result<(), MempoolError> {
        self.sender
            .send((tx.into_received(Received::P2P), None))
            .map_err(|err| err.into())
    }
}

//Manage send from the RPC source
impl TxEventSender<RpcSender> {
    pub fn build(sender: UnboundedSender<(Transaction<Received>, Option<CallbackSender>)>) -> Self {
        TxEventSender {
            sender,
            _marker: PhantomData,
        }
    }

    pub async fn send_tx(&self, tx: Transaction<Created>) -> Result<(), MempoolError> {
        let (sender, rx) = oneshot::channel();
        self.sender
            .send((tx.into_received(Received::RPC), Some(sender)))
            .map_err(MempoolError::from)?;
        rx.await?
    }
}

//Manage send from the Tx result execution source
impl TxEventSender<TxResultSender> {
    pub fn build(sender: UnboundedSender<(Transaction<Received>, Option<CallbackSender>)>) -> Self {
        TxEventSender {
            sender,
            _marker: PhantomData,
        }
    }

    pub async fn send_tx(&self, tx: Transaction<Created>) -> Result<(), MempoolError> {
        let (sender, rx) = oneshot::channel();
        self.sender
            .send((tx.into_received(Received::TXRESULT), Some(sender)))
            .map_err(MempoolError::from)?;
        rx.await?
    }
}

// `ValidatedTxReceiver` provides a simple trait to decouple event based
// Transaction handling from the execution part.
#[async_trait::async_trait]
pub trait ValidatedTxReceiver: Send + Sync {
    async fn send_new_tx(&mut self, tx: Transaction<Validated>) -> eyre::Result<()>;
}

#[async_trait]
pub trait ValidateStorage: Send + Sync {
    async fn get_tx(&self, hash: &Hash) -> eyre::Result<Option<Transaction<Validated>>>;
    async fn contains_program(&self, hash: Hash) -> eyre::Result<bool>;
}

#[async_trait]
pub trait Storage: Send + Sync {
    async fn get(&self, hash: &Hash) -> Result<Option<Transaction<Validated>>>;
    async fn set(&self, tx: &Transaction<Validated>) -> Result<()>;
    async fn fill_deque(&self, deque: &mut VecDeque<Transaction<Validated>>) -> Result<()>;
}

#[async_trait]
pub trait MempoolStorage: Storage + acl::AclWhitelist + ValidateStorage {}

#[derive(Clone)]
pub struct Mempool {
    storage: Arc<dyn MempoolStorage>,
    deque: VecDeque<Transaction<Validated>>,
}

impl Mempool {
    pub async fn new(storage: Arc<dyn MempoolStorage>) -> Result<Self> {
        let mut deque = VecDeque::new();
        storage.fill_deque(&mut deque).await?;
        Ok(Self { storage, deque })
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn start_tx_validation_event_loop(
        local_directory_path: PathBuf,
        bind_addr: SocketAddr,
        http_download_port: u16,
        http_peer_list: Arc<tokio::sync::RwLock<HashMap<SocketAddr, Option<u16>>>>,
        // Used to receive new transactions that arrive to the node from the outside.
        rcv_tx_event_rx: UnboundedReceiver<(Transaction<Received>, Option<CallbackSender>)>,
        mempool: Arc<RwLock<Mempool>>,
        storage: Arc<impl ValidateStorage + 'static>,
        acl_whitelist: Arc<impl acl::AclWhitelist + 'static>,
    ) -> eyre::Result<(
        JoinHandle<()>,
        // Output stream used to propagate transactions.
        impl Stream<Item = Transaction<Validated>>,
    )> {
        // Start Tx validation event loop.
        txvalidation::spawn_event_loop(
            local_directory_path,
            bind_addr,
            http_download_port,
            http_peer_list,
            rcv_tx_event_rx,
            mempool,
            storage,
            acl_whitelist,
        )
        .await
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

        tracing::trace!("mempool add Tx done");

        Ok(())
    }

    pub fn size(&self) -> usize {
        self.deque.len()
    }
}

#[async_trait::async_trait]
impl ValidatedTxReceiver for Mempool {
    async fn send_new_tx(&mut self, tx: Transaction<Validated>) -> eyre::Result<()> {
        self.add(tx).await
    }
}
