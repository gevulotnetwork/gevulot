use crate::txvalidation::download_manager;
use crate::txvalidation::EventProcessError;
use crate::types::transaction::Payload;
use crate::types::transaction::TransactionError;
use crate::types::Hash;
use crate::types::Signature;
use crate::types::Transaction;
use crate::Mempool;
use futures::future::join_all;
use futures_util::TryFutureExt;
use gevulot_node::types::transaction::AclWhitelist;
use libsecp256k1::verify;
use libsecp256k1::Message;
use libsecp256k1::PublicKey;
use sha3::{Digest, Sha3_256};
use std::collections::HashSet;
use std::fmt::Debug;
use std::net::SocketAddr;
use std::path::PathBuf;
use tokio::sync::mpsc::UnboundedSender;

///Content of all Tx.
#[derive(Debug, Clone)]
pub struct EventTxContent {
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

//Marker type to define the event.
#[derive(Debug)]
pub enum SourceTxType {
    P2P,
    RPC,
    TXRESULT,
}

#[derive(Debug)]
struct ConsistentTxType;

//Event processing depends on the marker type.
#[derive(Debug, Clone)]
pub struct EventTx<T: Debug> {
    pub content: EventTxContent,
    pub tx_type: T,
}

impl<T: Debug> EventTx<T> {
    //helper method to transition from one event to another.
    fn transition<N: Debug>(&self, tx_type: N) -> EventTx<N> {
        EventTx {
            content: self.content.clone(),
            tx_type,
        }
    }
}

//Processing of event that arrive: SourceTxType.
impl EventTx<SourceTxType> {
    //Tx validation process.
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
                tx_type: ConsistentTxType,
            })),
            SourceTxType::TXRESULT => Some(PropagateTx(EventTx {
                content: self.content,
                tx_type: ConsistentTxType,
            })),
        }
    }
}

//RcvTx processing
pub struct RcvTx(pub EventTx<SourceTxType>);
impl RcvTx {
    pub async fn process_event(
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

//Download Tx processing
pub struct DownloadTx(EventTx<SourceTxType>);
impl DownloadTx {
    pub async fn process_event(
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
            .map(|asset_file| {
                download_manager::download_asset_file(
                    local_directory_path,
                    &http_peer_list,
                    &http_client,
                    asset_file,
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
        let newtx = NewTx(self.0.transition(ConsistentTxType));
        let propagate = self.0.propagate();
        Ok((newtx, propagate))
    }
}

//Propagate Tx processing
pub struct PropagateTx(EventTx<ConsistentTxType>);
impl PropagateTx {
    pub async fn process_event(
        self,
        p2p_sender: &UnboundedSender<Transaction>,
    ) -> Result<(), EventProcessError> {
        let tx: Transaction = self.0.content.into();
        tracing::info!("Tx validation propagate tx:{}", tx.hash.to_string());
        p2p_sender.send(tx).map_err(|err| err.into())
    }
}

//Save new Tx processing
pub struct NewTx(EventTx<ConsistentTxType>);
impl NewTx {
    pub async fn process_event(self, mempool: &mut Mempool) -> Result<(), EventProcessError> {
        let tx: Transaction = self.0.content.into();
        tracing::info!("Tx validation save tx:{}", tx.hash.to_string());
        mempool
            .add(tx)
            .map_err(|err| EventProcessError::SaveTxError(format!("{err}")))
            .await
    }
}
