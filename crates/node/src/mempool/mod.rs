use crate::mempool::preexec::PreExecTx;
use crate::mempool::preexec::PreexecError;
use crate::mempool::preexec::{TxCache, TxPreExecEvent};
use crate::mempool::validate::ReceivedTx;
use crate::mempool::validate::TxValidateEvent;
use crate::mempool::validate::ValidateError;
use crate::types::Program;
use crate::types::{
    transaction::{Created, Execute, Received, Validated},
    Hash, Transaction,
};
use async_trait::async_trait;
use eyre::Result;
use futures::stream::FuturesUnordered;
use futures_util::Stream;
use futures_util::StreamExt;
use futures_util::TryFutureExt;
use gevulot_node::acl;
use std::collections::HashMap;
use std::fmt::Debug;
use std::marker::PhantomData;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use thiserror::Error;
use tokio::select;
use tokio::sync::mpsc;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::oneshot;
use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tokio_stream::wrappers::UnboundedReceiverStream;

mod download_manager;
mod preexec;
mod validate;

#[allow(clippy::enum_variant_names)]
#[derive(Error, Debug)]
pub enum MempoolError {
    #[error("Error during Tx validation")]
    EventValidateProcess(#[from] ValidateError),
    #[error("Error during Tx pre execute processing")]
    EventPreExecuteProcess(#[from] PreexecError),
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

// `TxValidateEventSender` holds the received transaction of a specific state together with an optional callback interface.
#[derive(Debug, Clone)]
pub struct TxValidateEventSender<T> {
    sender: UnboundedSender<(Transaction<Received>, Option<CallbackSender>)>,
    _marker: PhantomData<T>,
}

//Manage send from the p2p source
impl TxValidateEventSender<P2pSender> {
    pub fn build(sender: UnboundedSender<(Transaction<Received>, Option<CallbackSender>)>) -> Self {
        TxValidateEventSender {
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
impl TxValidateEventSender<RpcSender> {
    pub fn build(sender: UnboundedSender<(Transaction<Received>, Option<CallbackSender>)>) -> Self {
        TxValidateEventSender {
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
impl TxValidateEventSender<TxResultSender> {
    pub fn build(sender: UnboundedSender<(Transaction<Received>, Option<CallbackSender>)>) -> Self {
        TxValidateEventSender {
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
}

#[async_trait]
pub trait MempoolStorage: Storage + acl::AclWhitelist + ValidateStorage {
    async fn get_unexecuted_transactions(&self) -> Result<Vec<Transaction<Validated>>>;
}

#[derive(Clone)]
pub struct Mempool {
    storage: Arc<dyn MempoolStorage>,
}

impl Mempool {
    pub async fn new(storage: Arc<dyn MempoolStorage>) -> Result<Self> {
        Ok(Self { storage })
    }

    // Tx process:
    //  * verify Tx (sign / whitelist)
    //  * download Tx assets
    //  * propagate Tx
    //  * Prepare Tx for execution: Wait program and Parent dep.
    //  * Save Tx: the Tx is consistent and executable
    //  * Send Tx to execution.
    //  * Save Tx execution result (Not already done)
    #[allow(clippy::too_many_arguments)]
    pub async fn start_tx_validation_event_loop(
        local_directory_path: PathBuf,
        bind_addr: SocketAddr,
        http_download_port: u16,
        http_peer_list: Arc<tokio::sync::RwLock<HashMap<SocketAddr, Option<u16>>>>,
        // Used to receive new transactions that arrive to the node from the outside.
        mut rcv_tx_event_rx: UnboundedReceiver<(Transaction<Received>, Option<CallbackSender>)>,
        mempool: Arc<RwLock<Mempool>>,
        storage: Arc<impl ValidateStorage + 'static>,
        acl_whitelist: Arc<impl acl::AclWhitelist + 'static>,
    ) -> eyre::Result<(
        // Output stream used to propagate Tx to p2p.
        impl Stream<Item = Transaction<Validated>>,
        // Output stream used to send transactions to execution.
        mpsc::UnboundedReceiver<Transaction<Execute>>,
        JoinHandle<()>,
    )> {
        let (p2p_sender, p2p_recv) = mpsc::unbounded_channel::<Transaction<Validated>>();
        // The channel should never be full or there's a back pressure issue. Generate a watchdog error.
        // If the channel is full buffer the pending Tx.
        let (execute_tx_sender, execute_tx_recv) =
            mpsc::unbounded_channel::<Transaction<Execute>>();

        // Get unprocessed Tx from the DB and send them to execution.
        {
            let mempool = mempool.write().await;
            for tx in mempool.storage.get_unexecuted_transactions().await? {
                if let Ok(exec_tx) = tx.try_into() {
                    if let Err(err) = execute_tx_sender.send(exec_tx) {
                        tracing::error!("Mempool error during sending Tx to execution:{err}");
                    }
                }
            }
        }

        let jh = tokio::spawn({
            let mut program_wait_tx_cache = TxCache::<Program>::new();
            let mut parent_wait_tx_cache = TxCache::<Transaction<Validated>>::new();
            let mut preexecute_txs_futures = FuturesUnordered::new();

            async move {
                loop {
                    select! {
                        // Receive new Tx process.
                        Some((tx, callback)) = rcv_tx_event_rx.recv() => {
                            let http_peer_list = validate::convert_peer_list_to_vec(&http_peer_list).await;
                            let p2p_sender = p2p_sender.clone();

                            let event: TxValidateEvent<ReceivedTx> = tx.into();

                            let validate_jh = tokio::spawn({
                                let local_directory_path = local_directory_path.clone();
                                let acl_whitelist = acl_whitelist.clone();
                                async move {
                                    event
                                        .verify_tx(acl_whitelist.as_ref())
                                        .and_then(|download_event| {
                                            download_event.downlod_tx_assets(&local_directory_path, http_peer_list)
                                        })
                                        .and_then(|(new_tx, propagate_tx)| async move {
                                            if let Some(propagate_tx) = propagate_tx {
                                                propagate_tx.propagate_tx(&p2p_sender).await?;
                                            }
                                            let pre_exec_tx_event: TxPreExecEvent<PreExecTx> = new_tx.tx.into();
                                            Ok(pre_exec_tx_event)
                                        })
                                        .await
                                }
                            });
                            let fut = validate_jh
                                //.or_else(|err| async move {Err(ValidateError::TxValidateError(format!("{err}")))} )
                                .and_then(|res| async move {Ok((res,callback))});
                            preexecute_txs_futures.push(fut);

                        }
                        // Verify Tx parent dependency, save and send to execution scheduler all ready Tx.
                        Some(Ok((validate_tx_res, callback))) = preexecute_txs_futures.next() =>  {
                            let preexec_res = match validate_tx_res {
                                Err(err) => {
                                    let msg = format!("Error during verify tx :{err}");
                                    tracing::error!("{}", &msg);
                                    Err(PreexecError::PreProcessError(msg))
                                }
                                Ok(pre_exec_tx_event) => {
                                    let mempool = mempool.write().await;
                                    let execute_tx_sender = execute_tx_sender.clone();

                                    // Validate all Tx deps.
                                    pre_exec_tx_event
                                        .validate_tx_dep(
                                            &mut program_wait_tx_cache,
                                            &mut parent_wait_tx_cache,
                                            storage.as_ref(),
                                        )
                                        .and_then(|mut new_txs| async move {
                                            // Save Tx
                                            // Save new Tx in the main loop.
                                            // To avoid when there's a lot of waiting Txs freed by one Tx
                                            // and an arriving Tx that depend on is saved before and generate a db constrain error.
                                            // Can lock the loop during the save.
                                            // The unbounded channel rcv_tx_event_rx will buffer the Tx waiting.
                                            let mut res = Ok(());
                                            for new_tx in new_txs.drain(..) {
                                                tracing::info!(
                                                    "Tx validation save in db tx:{}  payload:{}",
                                                    new_tx.tx.hash.to_string(),
                                                    new_tx.tx.payload
                                                );
                                                if let Err(err) =  mempool.storage.set(&new_tx.tx).await {
                                                    tracing::error!("Error during validate tx saving :{err}");
                                                    res = Err(PreexecError::StorageError(err.to_string()));
                                                }

                                                //send Tx to execute
                                                if let Ok(exec_tx) = new_tx.try_into() {
                                                    if let Err(err) = execute_tx_sender.send(exec_tx) {
                                                        tracing::error!("Mempool error during sending Tx to execution:{err}");
                                                    }
                                                }

                                            }
                                            res
                                        }).await
                                }
                            };

                            if let Err(ref err) = preexec_res {
                                tracing::error!("Error during pre exec process:{}", err);
                            }

                            // The callback return call must be made after the Tx is saved in the DB.
                            // Otherwise Tx query via RPC return a not found until the save is done.
                            // If the save is move before pre execute, this call can be move too.
                            if let Some(callback) = callback {
                                // Notify Tx sender the verification process resutl.
                                let callback_result = preexec_res.clone().map(|_|()).map_err(|err|err.into());
                                let _ = callback.send(callback_result);
                            }
                        }
                    }
                }
            }
        });
        let p2p_stream = UnboundedReceiverStream::new(p2p_recv);
        // let execute_tx_stream = UnboundedReceiverStream::new(execute_tx_recv);
        Ok((p2p_stream, execute_tx_recv, jh))
    }
}

#[async_trait::async_trait]
impl ValidatedTxReceiver for Mempool {
    async fn send_new_tx(&mut self, tx: Transaction<Validated>) -> eyre::Result<()> {
        self.storage.set(&tx).await
    }
}
