use crate::types::transaction::Payload;
use crate::types::transaction::TransactionError;
use crate::types::Hash;
use crate::types::Signature;
use crate::types::Transaction;
use crate::Mempool;
use futures::future::join_all;
use futures_util::stream::FuturesUnordered;
use futures_util::Stream;
use futures_util::StreamExt;
use futures_util::TryFutureExt;
use gevulot_node::types::transaction::AclWhitelist;
use libsecp256k1::verify;
use libsecp256k1::Message;
use libsecp256k1::PublicKey;
use sha3::{Digest, Sha3_256};
use std::collections::HashMap;
use std::collections::HashSet;
use std::fmt::Debug;
use std::marker::PhantomData;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::mpsc;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::oneshot;
use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tokio_stream::wrappers::UnboundedReceiverStream;

mod download_manager;

#[derive(Error, Debug)]
pub enum EventProcessError {
    #[error("Fail to rcv Tx from the channel: {0}")]
    RcvChannelError(#[from] tokio::sync::oneshot::error::RecvError),
    #[error("Fail to send the Tx on the channel: {0}")]
    SendChannelError(#[from] tokio::sync::mpsc::error::SendError<(RcvTx, Option<CallbackSender>)>),
    #[error("Fail to send the Tx on the channel: {0}")]
    PropagateTxError(#[from] tokio::sync::mpsc::error::SendError<Transaction>),
    #[error("validation fail: {0}")]
    ValidateError(String),
    #[error("Tx asset fail to download because {0}")]
    DownloadAssetError(String),
    #[error("Save Tx error: {0}")]
    SaveTxError(String),
}

pub type CallbackSender = oneshot::Sender<Result<(), EventProcessError>>;

///Content of all Tx.
#[derive(Debug, Clone)]
struct EventTxContent {
    pub author: PublicKey,
    pub hash: Hash,
    pub payload: Payload,
    pub nonce: u64,
    pub signature: Signature,
}

impl From<Transaction> for EventTxContent {
    fn from(tx: Transaction) -> Self {
        EventTxContent {
            author: tx.author,
            hash: tx.hash,
            payload: tx.payload,
            nonce: tx.nonce,
            signature: tx.signature,
        }
    }
}

impl From<EventTxContent> for Transaction {
    fn from(content: EventTxContent) -> Self {
        Transaction {
            author: content.author,
            hash: content.hash,
            payload: content.payload,
            nonce: content.nonce,
            signature: content.signature,
            propagated: true,
            executed: false,
        }
    }
}

//event base type.
#[derive(Debug, Clone)]
pub struct EventTx<T: Debug> {
    content: EventTxContent,
    tx_type: T,
}

impl<T: Debug> EventTx<T> {
    fn transition_new<N: Debug>(&self, tx_type: N) -> EventTx<N> {
        EventTx {
            content: self.content.clone(),
            tx_type,
        }
    }
    fn transition<N: Debug>(self, tx_type: N) -> EventTx<N> {
        EventTx {
            content: self.content,
            tx_type,
        }
    }
}

#[derive(Debug)]
enum SourceTxType {
    P2P,
    RPC,
    TXRESULT,
}

impl EventTx<SourceTxType> {
    async fn validate_tx(&self, acl_whitelist: &impl AclWhitelist) -> Result<(), TransactionError> {
        let tx: Transaction = self.content.clone().into();
        tx.validate()?;

        // Secondly verify that author is whitelisted.
        if !acl_whitelist.contains(&tx.author).await? {
            return Err(TransactionError::Validation(
                "Tx permission denied signer not authorized".to_string(),
            ));
        }

        if let Payload::Run { ref workflow } = self.content.payload {
            let mut programs = HashSet::new();
            for step in &workflow.steps {
                if !programs.insert(step.program) {
                    return Err(TransactionError::Validation(format!(
                        "multiple programs in workflow: {}",
                        &step.program
                    )));
                }
            }
        }

        Ok(())
    }

    fn verify_tx_signature(&self) -> bool {
        let mut hasher = Sha3_256::new();
        let mut buf = vec![];
        hasher.update(self.content.author.serialize());
        self.content.payload.serialize_into(&mut buf);
        hasher.update(buf);
        hasher.update(self.content.nonce.to_be_bytes());

        let hash: Hash = (&hasher.finalize()[0..32]).into();
        let msg: Message = hash.into();
        verify(&msg, &self.content.signature.into(), &self.content.author)
    }

    fn propagate(self) -> Option<PropagateTx> {
        match self.tx_type {
            SourceTxType::P2P => None,
            SourceTxType::RPC => Some(PropagateTx(EventTx {
                content: self.content,
                tx_type: ConsistentTx,
            })),
            SourceTxType::TXRESULT => Some(PropagateTx(EventTx {
                content: self.content,
                tx_type: ConsistentTx,
            })),
        }
    }
}

#[derive(Debug)]
struct ConsistentTx;

//event list
pub struct RcvTx(EventTx<SourceTxType>);
impl RcvTx {
    async fn process_event(
        self,
        acl_whitelist: &impl AclWhitelist,
    ) -> Result<DownloadTx, EventProcessError> {
        match self.0.validate_tx(acl_whitelist).await {
            Ok(()) => Ok(DownloadTx(self.0)),
            Err(err) => Err(EventProcessError::ValidateError(format!(
                "Tx validation fail:{err}"
            ))),
        }
    }
}
struct DownloadTx(EventTx<SourceTxType>);
impl DownloadTx {
    async fn process_event(
        self,
        local_directory_path: &PathBuf,
        http_peer_list: Vec<(SocketAddr, Option<u16>)>,
    ) -> Result<(NewTx, Option<PropagateTx>), EventProcessError> {
        let tx_hash = self.0.content.hash;
        let http_client = reqwest::Client::new();
        let asset_file_list = self
            .0
            .content
            .payload
            .get_asset_list(tx_hash)
            .map_err(|err| {
                EventProcessError::DownloadAssetError(format!(
                    "Asset file param conversion error:{err}"
                ))
            })?;

        let futures: Vec<_> = asset_file_list
            .into_iter()
            .map(|(asset_file, checksum)| {
                download_manager::download_asset_file(
                    local_directory_path,
                    &http_peer_list,
                    &http_client,
                    asset_file,
                    checksum,
                )
            })
            .collect();
        join_all(futures)
            .await
            .into_iter()
            .map(|res| res)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|err| {
                EventProcessError::DownloadAssetError(format!("Exwecution error:{err}"))
            })?;
        let newtx = NewTx(self.0.transition_new(ConsistentTx));
        let propagate = self.0.propagate();
        Ok((newtx, propagate))
    }
}
struct PropagateTx(EventTx<ConsistentTx>);
impl PropagateTx {
    async fn process_event(
        self,
        p2p_sender: &UnboundedSender<Transaction>,
    ) -> Result<(), EventProcessError> {
        let tx: Transaction = self.0.content.into();
        p2p_sender.send(tx).map_err(|err| err.into())
    }
}

struct NewTx(EventTx<ConsistentTx>);
impl NewTx {
    async fn process_event(self, mempool: &mut Mempool) -> Result<(), EventProcessError> {
        let tx: Transaction = self.0.content.into();
        mempool
            .add(tx)
            .map_err(|err| EventProcessError::SaveTxError(format!("{err}")))
            .await
    }
}

//send Tx interface with outside the module
pub struct RpcSender;
#[derive(Clone)]
pub struct P2pSender;
pub struct TxResultSender;

#[derive(Debug, Clone)]
pub struct TxEventSender<T> {
    sender: UnboundedSender<(RcvTx, Option<CallbackSender>)>,
    _marker: PhantomData<T>,
}

impl TxEventSender<P2pSender> {
    pub fn build(sender: UnboundedSender<(RcvTx, Option<CallbackSender>)>) -> Self {
        TxEventSender {
            sender,
            _marker: PhantomData,
        }
    }

    pub fn send_tx(&self, tx: Transaction) -> Result<(), EventProcessError> {
        let event = EventTx {
            content: tx.into(),
            tx_type: SourceTxType::P2P,
        };
        self.sender
            .send((RcvTx(event), None))
            .map_err(|err| err.into())
    }
}

impl TxEventSender<RpcSender> {
    pub fn build(sender: UnboundedSender<(RcvTx, Option<CallbackSender>)>) -> Self {
        TxEventSender {
            sender,
            _marker: PhantomData,
        }
    }

    pub async fn send_tx(&self, tx: Transaction) -> Result<(), EventProcessError> {
        let (sender, rx) = oneshot::channel();
        let event = EventTx {
            content: tx.into(),
            tx_type: SourceTxType::RPC,
        };
        self.sender
            .send((RcvTx(event), Some(sender)))
            .map_err(|err| EventProcessError::from(err))?;
        rx.await?
    }
}

impl TxEventSender<TxResultSender> {
    pub fn build(sender: UnboundedSender<(RcvTx, Option<CallbackSender>)>) -> Self {
        TxEventSender {
            sender,
            _marker: PhantomData,
        }
    }

    pub async fn send_tx(&self, tx: Transaction) -> Result<(), EventProcessError> {
        let (sender, rx) = oneshot::channel();
        let event = EventTx {
            content: tx.into(),
            tx_type: SourceTxType::TXRESULT,
        };
        self.sender
            .send((RcvTx(event), Some(sender)))
            .map_err(|err| EventProcessError::from(err))?;
        rx.await?
    }
}

pub async fn start_event_loop(
    local_directory_path: PathBuf,
    bind_addr: SocketAddr,
    http_download_port: u16,
    http_peer_list: Arc<tokio::sync::RwLock<HashMap<SocketAddr, Option<u16>>>>,
    acl_whitelist: Arc<impl AclWhitelist + 'static>,
    mempool: Arc<RwLock<Mempool>>,
) -> eyre::Result<(
    JoinHandle<()>,
    UnboundedSender<(RcvTx, Option<CallbackSender>)>,
    impl Stream<Item = Transaction>,
)> {
    let local_directory_path = Arc::new(local_directory_path);
    //start http download manager
    let download_jh =
        download_manager::serve_files(bind_addr, http_download_port, local_directory_path.clone())
            .await?;

    let (tx, mut rcv_tx_event_rx) = mpsc::unbounded_channel::<(RcvTx, Option<CallbackSender>)>();

    let (p2p_sender, p2p_recv) = mpsc::unbounded_channel::<Transaction>();
    let p2p_stream = UnboundedReceiverStream::new(p2p_recv);
    let jh = tokio::spawn({
        let local_directory_path = local_directory_path.clone();

        async move {
            while let Some((event, callback)) = rcv_tx_event_rx.recv().await {
                //process RcvTx(EventTx<SourceTxType>) event
                let http_peer_list = convert_peer_list_to_vec(&http_peer_list).await;
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
