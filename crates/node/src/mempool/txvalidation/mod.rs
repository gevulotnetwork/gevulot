use crate::mempool::acl;
use crate::mempool::event::EventProcessError;
use crate::mempool::event::TxCache;
use crate::mempool::event::{ReceivedTx, TxEvent};
use crate::mempool::CallbackSender;
use crate::mempool::ValidateStorage;
use crate::mempool::ValidatedTxReceiver;
use crate::types::{
    transaction::{Received, Validated},
    Program, Transaction,
};
use futures::stream::FuturesUnordered;
use futures_util::Stream;
use futures_util::TryFutureExt;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::select;
use tokio::sync::mpsc::{self, UnboundedReceiver};
use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tokio_stream::wrappers::UnboundedReceiverStream;
use tokio_stream::StreamExt;

pub mod download_manager;

//Main event processing loog.
#[allow(clippy::too_many_arguments)]
pub async fn spawn_event_loop(
    local_directory_path: PathBuf,
    bind_addr: SocketAddr,
    http_download_port: u16,
    http_peer_list: Arc<tokio::sync::RwLock<HashMap<SocketAddr, Option<u16>>>>,
    // Used to receive new transactions that arrive to the node from the outside.
    mut rcv_tx_event_rx: UnboundedReceiver<(Transaction<Received>, Option<CallbackSender>)>,
    // Endpoint where validated transactions are sent to. Usually configured with Mempool.
    new_tx_receiver: Arc<RwLock<dyn ValidatedTxReceiver + 'static>>,
    storage: Arc<impl ValidateStorage + 'static>,
    acl_whitelist: Arc<impl acl::AclWhitelist + 'static>,
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
                                    .verify_tx(acl_whitelist.as_ref())
                                    .and_then(|download_event| {
                                        download_event.downlod_tx_assets(&local_directory_path, http_peer_list)
                                    })
                                    .and_then(|(wait_tx, propagate_tx)| async move {
                                        if let Some(propagate_tx) = propagate_tx {
                                            propagate_tx.propagate_tx(&p2p_sender).await?;
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
                    // Verify Tx parent dependency and send to mempool all ready Tx.
                    Some(Ok((wait_tx_res, callback))) = validated_txs_futures.next() =>  {
                        match wait_tx_res {
                            Ok(wait_tx) => {
                               match wait_tx.validate_tx_dep(&mut program_wait_tx_cache, &mut parent_wait_tx_cache, storage.as_ref()).await {
                                    Ok(new_tx_list) => {
                                        // Process new Tx in the main loop
                                        // to avoid when there's a lot of waiting Txs free by one Tx,
                                        // an arriving Tx that depend on a just free one is saved before and generate a db constrain error.
                                        // Can lock the loop during the save.
                                        // The unbounded channel will buffer the Tx waiting.
                                        let mut res = Ok(());
                                        for new_tx in new_tx_list {
                                           if let Err(err) =  new_tx.save_tx(&mut *(new_tx_receiver.write().await)).await {
                                                tracing::error!("Error during validate save tx process_event :{err}");
                                                res = Err(err);
                                           }
                                        }
                                        if let Some(callback) = callback {
                                            // Forget the result because if the RPC connection is closed the send can fail.
                                            let _ = callback.send(res.map_err(|err|err.into()));
                                        }
                                    }
                                    Err(err) => {
                                        tracing::error!("Error during Tx dependency verification :{err}");
                                        if let Some(callback) = callback {
                                            // Forget the result because if the RPC connection is closed the send can fail.
                                            let _ = callback.send(Err(err.into()));
                                        }
                                    }
                                }

                            }
                            Err(err)  => {
                                tracing::error!("Error during verify tx process_event :{err}");
                                if let Some(callback) = callback {
                                    // Forget the result because if the RPC connection is closed the send can fail.
                                    let _ = callback.send(Err(err.into()));
                                }
                            }
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
