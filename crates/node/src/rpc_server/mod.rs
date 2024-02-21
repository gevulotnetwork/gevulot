use crate::txvalidation::RpcSender;
use crate::txvalidation::TxEventSender;
use std::rc::Rc;
use std::{net::SocketAddr, sync::Arc};

use crate::{
    cli::Config,
    storage::Database,
    types::{
        transaction::{Created, Validated},
        Transaction,
    },
};
use eyre::Result;
use gevulot_node::types::{
    rpc::{RpcError, RpcResponse},
    Hash, TransactionTree,
};
use jsonrpsee::{
    server::{RpcModule, Server, ServerHandle},
    types::Params,
};

struct Context {
    database: Arc<Database>,
    tx_sender: TxEventSender<RpcSender>,
}

impl std::fmt::Debug for Context {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "RPC Context")
    }
}

pub struct RpcServer {
    local_addr: SocketAddr,
    server_handle: ServerHandle,
}

impl RpcServer {
    pub async fn run(
        cfg: Arc<Config>,
        database: Arc<Database>,
        tx_sender: TxEventSender<RpcSender>,
    ) -> Result<Self> {
        let server = Server::builder().build(cfg.json_rpc_listen_addr).await?;
        let mut module = RpcModule::new(Context {
            database,
            tx_sender,
        });

        module.register_async_method("sendTransaction", send_transaction)?;
        module.register_async_method("getTransaction", get_transaction)?;
        module.register_async_method("getTransactionTree", get_tx_tree)?;

        let local_addr = server.local_addr().unwrap();
        let server_handle = server.start(module);
        tracing::info!("listening for json-rpc at {}", local_addr);
        Ok(RpcServer {
            local_addr,
            server_handle,
        })
    }

    pub fn addr(&self) -> SocketAddr {
        self.local_addr
    }

    pub async fn stop(&self) -> Result<()> {
        self.server_handle.stop().map_err(|e| e.into())
    }
}

#[tracing::instrument(level = "info")]
async fn send_transaction(params: Params<'static>, ctx: Arc<Context>) -> RpcResponse<()> {
    tracing::info!("JSON-RPC: send_transaction()");

    // Real logic
    let tx: Transaction<Created> = match params.one() {
        Ok(tx) => tx,
        Err(e) => {
            tracing::error!("failed to parse transaction: {}", e);
            return RpcResponse::Err(RpcError::InvalidRequest(e.to_string()));
        }
    };

    if let Err(err) = ctx.tx_sender.send_tx(tx).await {
        tracing::error!("failed to persist transaction: {}", err);
        return RpcResponse::Err(RpcError::InvalidRequest(
            "failed to persist transaction".to_string(),
        ));
    }

    RpcResponse::Ok(())
}

#[tracing::instrument(level = "info")]
async fn get_transaction(
    params: Params<'static>,
    ctx: Arc<Context>,
) -> RpcResponse<Transaction<Validated>> {
    let tx_hash: Hash = match params.one() {
        Ok(tx_hash) => tx_hash,
        Err(e) => {
            tracing::error!("failed to parse transaction: {}", e);
            return RpcResponse::Err(RpcError::InvalidRequest(e.to_string()));
        }
    };

    tracing::info!("JSON-RPC: get_transaction()");

    match ctx.database.find_transaction(&tx_hash).await {
        Ok(Some(tx)) => RpcResponse::Ok(tx),
        Ok(None) => RpcResponse::Err(RpcError::NotFound(tx_hash.to_string())),
        Err(e) => RpcResponse::Err(RpcError::NotFound(tx_hash.to_string())),
    }
}

#[tracing::instrument(level = "info")]
async fn get_tx_tree(
    params: Params<'static>,
    ctx: Arc<Context>,
) -> RpcResponse<Rc<TransactionTree>> {
    let tx_hash: Hash = match params.one() {
        Ok(tx_hash) => tx_hash,
        Err(e) => {
            tracing::error!("failed to parse transaction: {}", e);
            return RpcResponse::Err(RpcError::InvalidRequest(e.to_string()));
        }
    };

    tracing::info!("JSON-RPC: get_tx_tree()");

    let txs = ctx
        .database
        .get_transaction_tree(&tx_hash)
        .await
        .expect("get_transaction_tree");

    // Root element is the one without parent.
    let root: Vec<Hash> = txs
        .iter()
        .filter_map(|x| if x.1.is_none() { Some(x.0) } else { None })
        .collect();

    if root.is_empty() {
        return RpcResponse::Err(RpcError::NotFound(format!(
            "no root tx found for {tx_hash}"
        )));
    } else if root.len() > 1 {
        return RpcResponse::Err(RpcError::InvalidRequest(format!(
            "more than one root elements found for {tx_hash}"
        )));
    }

    RpcResponse::Ok(build_tx_tree(&root[0], txs))
}

// `build_tx_tree` builds recursively a `TransactionTree` starting from `hash`
// and descending to its children. `txs` is a vector of
// <Tx hash, Option<Parent tx hash>> tuples.
fn build_tx_tree(hash: &Hash, txs: Vec<(Hash, Option<Hash>)>) -> Rc<TransactionTree> {
    let children = txs
        .iter()
        .filter_map(|x| {
            if let Some(leaf_hash) = x.1 {
                if &leaf_hash == hash {
                    return Some(build_tx_tree(&x.0, txs.clone()));
                }
            }
            None
        })
        .collect();

    let elem = txs.iter().find(|x| &x.0 == hash).unwrap();
    let tree_elem = if elem.1.is_none() {
        TransactionTree::Root {
            children,
            hash: *hash,
        }
    } else if children.is_empty() {
        TransactionTree::Leaf { hash: *hash }
    } else {
        TransactionTree::Node {
            children,
            hash: *hash,
        }
    };

    Rc::new(tree_elem)
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::txvalidation;
    use crate::txvalidation::CallbackSender;
    use crate::txvalidation::EventProcessError;
    use gevulot_node::types::transaction::Received;
    use jsonrpsee::{
        core::{client::ClientT, params::ArrayParams},
        http_client::HttpClientBuilder,
    };
    use libsecp256k1::SecretKey;
    use rand::{rngs::StdRng, SeedableRng};
    use std::{env::temp_dir, path::PathBuf};
    use tokio::sync::mpsc;
    use tokio::sync::mpsc::UnboundedReceiver;
    use tokio::sync::oneshot;
    use tracing_subscriber::{filter::LevelFilter, fmt::format::FmtSpan, EnvFilter};

    #[ignore]
    #[tokio::test]
    async fn test_send_transaction() {
        start_logger(LevelFilter::INFO);
        let (rpc_server, mut tx_receiver) = new_rpc_server().await;

        let url = format!("http://{}", rpc_server.addr());
        let rpc_client = HttpClientBuilder::default()
            .build(url)
            .expect("http client");

        let key = SecretKey::default();
        let mut tx = Transaction::default();
        tx.sign(&key);

        let mut params = ArrayParams::new();
        params.insert(&tx).expect("rpc params");

        let resp = rpc_client
            .request::<RpcResponse<()>, ArrayParams>("sendTransaction", params)
            .await
            .expect("rpc request");

        let recv_tx = tx_receiver.recv().await.expect("recv tx");

        let tx = Transaction {
            author: tx.author,
            hash: tx.hash,
            payload: tx.payload,
            nonce: tx.nonce,
            signature: tx.signature,
            propagated: tx.executed,
            executed: tx.executed,
            state: Received::RPC,
        };

        assert_eq!(tx, recv_tx.0);

        dbg!(resp);
    }

    #[test]
    fn test_build_tx_tree_as_expected() {
        let rng = &mut StdRng::from_entropy();
        let root = Hash::random(rng);
        let node1 = Hash::random(rng);
        let node2 = Hash::random(rng);
        let leaf1 = Hash::random(rng);
        let leaf2 = Hash::random(rng);
        let leaf3 = Hash::random(rng);
        let leaf4 = Hash::random(rng);

        let txs = vec![
            (root, None),
            (node1, Some(root)),
            (node2, Some(root)),
            (leaf1, Some(node1)),
            (leaf2, Some(node1)),
            (leaf3, Some(node2)),
            (leaf4, Some(node2)),
        ];

        let tree = build_tx_tree(&root, txs);
        let (ref root_hash, root_children) = match *tree {
            TransactionTree::Root { hash, ref children } => (hash, children),
            ref elem => panic!("invalid element type for tree root: {:#?}", elem),
        };

        assert_eq!(root_hash, &root);
        assert_eq!(root_children.len(), 2);

        if let TransactionTree::Node { ref children, hash } = **root_children.first().unwrap() {
            assert_eq!(hash, node1);
            assert_eq!(children.len(), 2);

            if let TransactionTree::Leaf { hash } = **children.first().unwrap() {
                assert_eq!(hash, leaf1);
            } else {
                panic!("expected TransactionTree::Leaf, got {:#?}", children[0]);
            }

            if let TransactionTree::Leaf { hash } = **children.get(1).unwrap() {
                assert_eq!(hash, leaf2);
            } else {
                panic!("expected TransactionTree::Leaf, got {:#?}", children[1]);
            }
        } else {
            panic!(
                "expected TransactionTree::Node, got {:#?}",
                root_children[0]
            );
        }

        if let TransactionTree::Node { ref children, hash } = **root_children.get(1).unwrap() {
            assert_eq!(hash, node2);
            assert_eq!(children.len(), 2);

            if let TransactionTree::Leaf { hash } = **children.first().unwrap() {
                assert_eq!(hash, leaf3);
            } else {
                panic!("expected TransactionTree::Leaf, got {:#?}", children[0]);
            }

            if let TransactionTree::Leaf { hash } = **children.get(1).unwrap() {
                assert_eq!(hash, leaf4);
            } else {
                panic!("expected TransactionTree::Leaf, got {:#?}", children[1]);
            }
        } else {
            panic!(
                "expected TransactionTree::Node, got {:#?}",
                root_children[1]
            );
        }
    }

    #[test]
    fn test_build_tx_tree_just_root() {
        let rng = &mut StdRng::from_entropy();
        let root = Hash::random(rng);

        let txs = vec![(root, None)];

        let tree = build_tx_tree(&root, txs);
        let (ref root_hash, root_children) = match *tree {
            TransactionTree::Root { hash, ref children } => (hash, children),
            ref elem => panic!("invalid element type for tree root: {:#?}", elem),
        };

        assert_eq!(root_hash, &root);
        assert!(root_children.is_empty());
    }

    #[test]
    fn test_build_tx_tree_one_node_one_leaf() {
        let rng = &mut StdRng::from_entropy();
        let root = Hash::random(rng);
        let node1 = Hash::random(rng);
        let leaf1 = Hash::random(rng);

        let txs = vec![(root, None), (node1, Some(root)), (leaf1, Some(node1))];

        let tree = build_tx_tree(&root, txs);
        let (ref root_hash, root_children) = match *tree {
            TransactionTree::Root { hash, ref children } => (hash, children),
            ref elem => panic!("invalid element type for tree root: {:#?}", elem),
        };

        assert_eq!(root_hash, &root);
        assert_eq!(root_children.len(), 1);

        if let TransactionTree::Node { ref children, hash } = **root_children.first().unwrap() {
            assert_eq!(hash, node1);
            assert_eq!(children.len(), 1);

            if let TransactionTree::Leaf { hash } = **children.first().unwrap() {
                assert_eq!(hash, leaf1);
            } else {
                panic!("expected TransactionTree::Leaf, got {:#?}", children[0]);
            }
        } else {
            panic!(
                "expected TransactionTree::Node, got {:#?}",
                root_children[0]
            );
        }
    }

    #[test]
    fn test_build_tx_tree_root_and_leaf() {
        let rng = &mut StdRng::from_entropy();
        let root = Hash::random(rng);
        let leaf1 = Hash::random(rng);

        let txs = vec![(root, None), (leaf1, Some(root))];

        let tree = build_tx_tree(&root, txs);
        let (ref root_hash, root_children) = match *tree {
            TransactionTree::Root { hash, ref children } => (hash, children),
            ref elem => panic!("invalid element type for tree root: {:#?}", elem),
        };

        assert_eq!(root_hash, &root);
        assert_eq!(root_children.len(), 1);

        if let TransactionTree::Leaf { hash } = **root_children.first().unwrap() {
            assert_eq!(hash, leaf1);
        } else {
            panic!(
                "expected TransactionTree::Leaf, got {:#?}",
                root_children[0]
            );
        }
    }

    fn start_logger(default_level: LevelFilter) {
        let filter = match EnvFilter::try_from_default_env() {
            Ok(filter) => filter,
            _ => EnvFilter::default().add_directive(default_level.into()),
        };

        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_span_events(FmtSpan::CLOSE)
            .with_target(false)
            .init();
    }

    async fn new_rpc_server() -> (
        RpcServer,
        UnboundedReceiver<(
            Transaction<Received>,
            Option<oneshot::Sender<Result<(), EventProcessError>>>,
        )>,
    ) {
        let cfg = Arc::new(Config {
            acl_whitelist_url: "http://127.0.0.1:0/does.not.exist".to_string(),
            data_directory: temp_dir(),
            db_url: "postgres://gevulot:gevulot@localhost/gevulot".to_string(),
            json_rpc_listen_addr: "127.0.0.1:0".parse().unwrap(),
            log_directory: temp_dir(),
            node_key_file: PathBuf::new().join("node.key"),
            p2p_discovery_addrs: vec![],
            p2p_listen_addr: "127.0.0.1:9999".parse().unwrap(),
            p2p_psk_passphrase: "secret.".to_string(),
            p2p_advertised_listen_addr: None,
            provider: "qemu".to_string(),
            vsock_listen_port: 8080,
            num_cpus: 8,
            mem_gb: 8,
            gpu_devices: None,
            http_download_port: 0,
        });

        let db = Arc::new(Database::new(&cfg.db_url).await.unwrap());

        let (sendtx, txreceiver) =
            mpsc::unbounded_channel::<(Transaction<Received>, Option<CallbackSender>)>();
        let txsender = txvalidation::TxEventSender::<txvalidation::RpcSender>::build(sendtx);

        (
            RpcServer::run(cfg, db, txsender)
                .await
                .expect("rpc_server.run"),
            txreceiver,
        )
    }
}
