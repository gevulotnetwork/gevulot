use crate::txvalidation::event::{EventTx, RcvTx};
use crate::types::{
    transaction::{TxCreate, TxReceive, TxValdiated},
    Transaction,
};
use crate::Mempool;
use futures_util::Stream;
use futures_util::TryFutureExt;
use gevulot_node::types::transaction::AclWhitelist;
use std::collections::HashMap;
use std::fmt::Debug;
use std::marker::PhantomData;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::mpsc;
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::oneshot;
use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tokio_stream::wrappers::UnboundedReceiverStream;

mod download_manager;
mod event;

#[allow(clippy::enum_variant_names)]
#[derive(Error, Debug)]
pub enum EventProcessError {
    #[error("Fail to rcv Tx from the channel: {0}")]
    RcvChannelError(#[from] tokio::sync::oneshot::error::RecvError),
    #[error("Fail to send the Tx on the channel: {0}")]
    SendChannelError(
        #[from]
        tokio::sync::mpsc::error::SendError<(Transaction<TxReceive>, Option<CallbackSender>)>,
    ),
    #[error("Fail to send the Tx on the channel: {0}")]
    PropagateTxError(#[from] Box<tokio::sync::mpsc::error::SendError<Transaction<TxValdiated>>>),
    #[error("validation fail: {0}")]
    ValidateError(String),
    #[error("Tx asset fail to download because {0}")]
    DownloadAssetError(String),
    #[error("Save Tx error: {0}")]
    SaveTxError(String),
}

pub type CallbackSender = oneshot::Sender<Result<(), EventProcessError>>;

//Sending Tx interface.
//Some marker type to define the sender source.
pub struct RpcSender;
#[derive(Clone)]
pub struct P2pSender;
pub struct TxResultSender;

//use to send a tx to the event process.
#[derive(Debug, Clone)]
pub struct TxEventSender<T> {
    sender: UnboundedSender<(Transaction<TxReceive>, Option<CallbackSender>)>,
    _marker: PhantomData<T>,
}

//Manage send from the p2p source
impl TxEventSender<P2pSender> {
    pub fn build(
        sender: UnboundedSender<(Transaction<TxReceive>, Option<CallbackSender>)>,
    ) -> Self {
        TxEventSender {
            sender,
            _marker: PhantomData,
        }
    }

    pub fn send_tx(&self, tx: Transaction<TxCreate>) -> Result<(), EventProcessError> {
        self.sender
            .send((tx.into_received(TxReceive::P2P), None))
            .map_err(|err| err.into())
    }
}

//Manage send from the RPC source
impl TxEventSender<RpcSender> {
    pub fn build(
        sender: UnboundedSender<(Transaction<TxReceive>, Option<CallbackSender>)>,
    ) -> Self {
        TxEventSender {
            sender,
            _marker: PhantomData,
        }
    }

    pub async fn send_tx(&self, tx: Transaction<TxCreate>) -> Result<(), EventProcessError> {
        let (sender, rx) = oneshot::channel();
        self.sender
            .send((tx.into_received(TxReceive::RPC), Some(sender)))
            .map_err(EventProcessError::from)?;
        rx.await?
    }
}

//Manage send from the Tx result execution source
impl TxEventSender<TxResultSender> {
    pub fn build(
        sender: UnboundedSender<(Transaction<TxReceive>, Option<CallbackSender>)>,
    ) -> Self {
        TxEventSender {
            sender,
            _marker: PhantomData,
        }
    }

    pub async fn send_tx(&self, tx: Transaction<TxCreate>) -> Result<(), EventProcessError> {
        let (sender, rx) = oneshot::channel();
        self.sender
            .send((tx.into_received(TxReceive::TXRESULT), Some(sender)))
            .map_err(EventProcessError::from)?;
        rx.await?
    }
}

//Main event processing loog.
pub async fn start_event_loop(
    local_directory_path: PathBuf,
    bind_addr: SocketAddr,
    http_download_port: u16,
    http_peer_list: Arc<tokio::sync::RwLock<HashMap<SocketAddr, Option<u16>>>>,
    acl_whitelist: Arc<impl AclWhitelist + 'static>,
    //New Tx are added to the mempool directly.
    //Like for the p2p a stream can be use to decouple both process.
    mempool: Arc<RwLock<Mempool>>,
) -> eyre::Result<(
    JoinHandle<()>,
    //channel use to send RcvTx event to the processing
    UnboundedSender<(Transaction<TxReceive>, Option<CallbackSender>)>,
    //output stream use to propagate Tx.
    impl Stream<Item = Transaction<TxValdiated>>,
)> {
    let local_directory_path = Arc::new(local_directory_path);
    //start http download manager
    let download_jh =
        download_manager::serve_files(bind_addr, http_download_port, local_directory_path.clone())
            .await?;

    let (tx, mut rcv_tx_event_rx) =
        mpsc::unbounded_channel::<(Transaction<TxReceive>, Option<CallbackSender>)>();

    let (p2p_sender, p2p_recv) = mpsc::unbounded_channel::<Transaction<TxValdiated>>();
    let p2p_stream = UnboundedReceiverStream::new(p2p_recv);
    let jh = tokio::spawn({
        let local_directory_path = local_directory_path.clone();

        async move {
            while let Some((tx, callback)) = rcv_tx_event_rx.recv().await {
                //create new event with the Tx
                let event: EventTx<RcvTx> = tx.into();

                //process RcvTx(EventTx<SourceTxType>) event
                let http_peer_list = convert_peer_list_to_vec(&http_peer_list).await;

                //process the receive event
                tokio::spawn({
                    let p2p_sender = p2p_sender.clone();
                    let local_directory_path = local_directory_path.clone();
                    let acl_whitelist = acl_whitelist.clone();
                    let mempool = mempool.clone();
                    async move {
                        let res = event
                            .process_event(acl_whitelist.as_ref())
                            .and_then(|download_event| {
                                download_event.process_event(&local_directory_path, http_peer_list)
                            })
                            .and_then(|(new_tx, propagate_tx)| async move {
                                if let Some(propagate_tx) = propagate_tx {
                                    propagate_tx.process_event(&p2p_sender).await?;
                                }
                                new_tx.process_event(&mut *(mempool.write().await)).await?;

                                Ok(())
                            })
                            .await;
                        //send the execution result back if needed.
                        if let Some(callback) = callback {
                            //forget the result because if the RPC connection is closed the send can fail.
                            let _ = callback.send(res);
                        }
                    }
                });
            }
        }
    });
    Ok((jh, tx, p2p_stream))
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
