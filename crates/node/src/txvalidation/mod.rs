use crate::txvalidation::event::TxCache;
use crate::txvalidation::event::{ReceivedTx, TxEvent};
use crate::types::{
    transaction::{Created, Received, Validated},
    Hash, Program, Transaction,
};
use async_trait::async_trait;
use futures::stream::FuturesUnordered;
use futures_util::Stream;
use futures_util::TryFutureExt;
use std::collections::HashMap;
use std::fmt::Debug;
use std::marker::PhantomData;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use thiserror::Error;
use tokio::select;
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::mpsc::{self, UnboundedReceiver};
use tokio::sync::oneshot;
use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tokio_stream::wrappers::UnboundedReceiverStream;
use tokio_stream::StreamExt;

pub mod acl;
mod download_manager;
mod event;

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

#[allow(clippy::enum_variant_names)]
#[derive(Error, Debug)]
pub enum EventProcessError {
    #[error("Fail to rcv Tx from the channel: {0}")]
    RcvChannelError(#[from] tokio::sync::oneshot::error::RecvError),
    #[error("Fail to send the Tx on the channel: {0}")]
    SendChannelError(
        #[from]
        tokio::sync::mpsc::error::SendError<(Transaction<Received>, Option<CallbackSender>)>,
    ),
    #[error("Fail to send the Tx on the channel: {0}")]
    PropagateTxError(#[from] Box<tokio::sync::mpsc::error::SendError<Transaction<Validated>>>),
    #[error("validation fail: {0}")]
    ValidateError(String),
    #[error("Tx asset fail to download because {0}")]
    DownloadAssetError(String),
    #[error("Save Tx error: {0}")]
    SaveTxError(String),
    #[error("Storage access error: {0}")]
    StorageError(String),
    #[error("AclWhite list authenticate error: {0}")]
    AclWhiteListAuthError(#[from] acl::AclWhiteListError),
}

pub type CallbackSender = oneshot::Sender<Result<(), EventProcessError>>;

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

    pub fn send_tx(&self, tx: Transaction<Created>) -> Result<(), EventProcessError> {
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

    pub async fn send_tx(&self, tx: Transaction<Created>) -> Result<(), EventProcessError> {
        let (sender, rx) = oneshot::channel();
        self.sender
            .send((tx.into_received(Received::RPC), Some(sender)))
            .map_err(EventProcessError::from)?;
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

    pub async fn send_tx(&self, tx: Transaction<Created>) -> Result<(), EventProcessError> {
        let (sender, rx) = oneshot::channel();
        self.sender
            .send((tx.into_received(Received::TXRESULT), Some(sender)))
            .map_err(EventProcessError::from)?;
        rx.await?
    }
}

//Main event processing loog.
#[allow(clippy::too_many_arguments)]
pub async fn spawn_event_loop(
    local_directory_path: PathBuf,
    bind_addr: SocketAddr,
    http_download_port: u16,
    http_peer_list: Arc<tokio::sync::RwLock<HashMap<SocketAddr, Option<u16>>>>,
    acl_whitelist: Arc<impl acl::AclWhitelist + 'static>,
    // Used to receive new transactions that arrive to the node from the outside.
    mut rcv_tx_event_rx: UnboundedReceiver<(Transaction<Received>, Option<CallbackSender>)>,
    // Endpoint where validated transactions are sent to. Usually configured with Mempool.
    new_tx_receiver: Arc<RwLock<dyn ValidatedTxReceiver + 'static>>,
    storage: Arc<impl ValidateStorage + 'static>,
) -> eyre::Result<(
    JoinHandle<()>,
    // Output stream used to propagate transactions.
    impl Stream<Item = Transaction<Validated>>,
)> {
    let local_directory_path = Arc::new(local_directory_path);
    // Start http download manager
    let download_jh =
        download_manager::serve_files(bind_addr, http_download_port, local_directory_path.clone())
            .await?;

    let (p2p_sender, p2p_recv) = mpsc::unbounded_channel::<Transaction<Validated>>();
    let p2p_stream = UnboundedReceiverStream::new(p2p_recv);
    let jh = tokio::spawn({
        let local_directory_path = local_directory_path.clone();
        let mut program_wait_tx_cache = TxCache::<Program>::new();
        let mut parent_wait_tx_cache = TxCache::<Transaction<Validated>>::new();
        let mut validated_txs_futures = FuturesUnordered::new();
        let mut validation_okresult_futures = FuturesUnordered::new();
        let mut validation_errresult_futures = FuturesUnordered::new();

        async move {
            loop {
                select! {
                    // Execute Tx verification in a separate task.
                    Some((tx, callback)) = rcv_tx_event_rx.recv() => {
                        //create new event with the Tx
                        let event: TxEvent<ReceivedTx> = tx.into();

                        // Process RcvTx(EventTx<SourceTxType>) event
                        let http_peer_list = convert_peer_list_to_vec(&http_peer_list).await;

                        tracing::trace!("txvalidation receive event:{}", event.tx.hash.to_string());

                        // Process the receive event
                        let validate_jh = tokio::spawn({
                            let p2p_sender = p2p_sender.clone();
                            let local_directory_path = local_directory_path.clone();
                            let acl_whitelist = acl_whitelist.clone();
                            async move {
                                event
                                    .process_event(acl_whitelist.as_ref())
                                    .and_then(|download_event| {
                                        download_event.process_event(&local_directory_path, http_peer_list)
                                    })
                                    .and_then(|(wait_tx, propagate_tx)| async move {
                                        if let Some(propagate_tx) = propagate_tx {
                                            propagate_tx.process_event(&p2p_sender).await?;
                                        }
                                        Ok(wait_tx)
                                    })
                                    .await
                            }
                        });
                        let fut = validate_jh
                            .or_else(|err| async move {Err(EventProcessError::ValidateError(format!("Process execution error:{err}")))} )
                            .and_then(|res| async move {Ok((res,callback))});
                        validated_txs_futures.push(fut);
                    }
                    // Verify Tx parent and send to mempool all ready Tx.
                    Some(Ok((wait_tx_res, callback))) = validated_txs_futures.next() =>  {
                        match wait_tx_res {
                            Ok(wait_tx) => {
                               match wait_tx.process_event(&mut program_wait_tx_cache, &mut parent_wait_tx_cache, storage.as_ref()).await {
                                    Ok(new_tx_list) => {
                                        //
                                        let jh  = tokio::spawn({
                                            let new_tx_receiver = new_tx_receiver.clone();
                                            async move {
                                                for new_tx in new_tx_list {
                                                   new_tx.process_event(&mut *(new_tx_receiver.write().await)).await?;
                                                }
                                                Ok(())
                                            }
                                        });
                                        let fut = jh
                                            .or_else(|err| async move {Err(EventProcessError::ValidateError(format!("Process execution error:{err}")))} )
                                            .and_then(|res| async move {Ok((res,callback))});
                                        validation_okresult_futures.push(fut);
                                    }
                                    Err(err) => {
                                        validation_errresult_futures.push(futures::future::ready((err, callback)));
                                    }
                                }

                            }
                            Err(err)  => {
                                validation_errresult_futures.push(futures::future::ready((err, callback)));
                            }
                        }
                     }
                     Some(Ok((res, callback))) = validation_okresult_futures.next() =>  {
                        if let Err(ref err) = res {
                            tracing::error!("Error during validate save tx process_event :{err}");
                        }
                        if let Some(callback) = callback {
                            // Forget the result because if the RPC connection is closed the send can fail.
                            let _ = callback.send(res);
                        }
                     }
                     Some((res, callback)) = validation_errresult_futures.next() =>  {
                        tracing::error!("Error during validate tx process_event :{res}");
                        if let Some(callback) = callback {
                            // Forget the result because if the RPC connection is closed the send can fail.
                            let _ = callback.send(Err(res));
                        }
                     }

                } // End select!
            } // End loop
        }
    });
    Ok((jh, p2p_stream))
}

async fn convert_peer_list_to_vec(
    http_peer_list: &tokio::sync::RwLock<HashMap<SocketAddr, Option<u16>>>,
) -> Vec<(SocketAddr, Option<u16>)> {
    http_peer_list
        .read()
        .await
        .iter()
        .map(|(a, p)| (*a, *p))
        .collect()
}
