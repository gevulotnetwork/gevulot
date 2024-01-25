use std::{net::SocketAddr, sync::Arc};

use eyre::Result;
use gevulot_node::types::{
    rpc::{RpcError, RpcResponse},
    Hash, TransactionTree,
};
use jsonrpsee::{
    server::{RpcModule, Server, ServerHandle},
    types::Params,
};
use tokio::sync::RwLock;

use crate::{
    asset_manager::AssetManager, cli::Config, mempool::Mempool, storage::Database,
    types::Transaction,
};

struct Context {
    database: Arc<Database>,
    mempool: Arc<RwLock<Mempool>>,
    asset_manager: Arc<AssetManager>,
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
        mempool: Arc<RwLock<Mempool>>,
        asset_manager: Arc<AssetManager>,
    ) -> Result<Self> {
        let server = Server::builder().build(cfg.json_rpc_listen_addr).await?;
        let mut module = RpcModule::new(Context {
            database,
            mempool,
            asset_manager,
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
    let tx: Transaction = match params.one() {
        Ok(tx) => tx,
        Err(e) => {
            tracing::error!("failed to parse transaction: {}", e);
            return RpcResponse::Err(RpcError::InvalidRequest(e.to_string()));
        }
    };

    if let Err(err) = ctx.mempool.write().await.add(tx.clone()).await {
        tracing::error!("failed to persist transaction: {}", err);
        return RpcResponse::Err(RpcError::InvalidRequest(
            "failed to persist transaction".to_string(),
        ));
    }

    if let Err(err) = ctx.asset_manager.handle_transaction(&tx).await {
        tracing::error!(
            "failed to enqueue transaction for asset processing: {}",
            err
        );
        return RpcResponse::Err(RpcError::InvalidRequest(
            "failed to enqueue transaction for asset processing".to_string(),
        ));
    }

    RpcResponse::Ok(())
}

#[tracing::instrument(level = "info")]
async fn get_transaction(params: Params<'static>, ctx: Arc<Context>) -> RpcResponse<Transaction> {
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
async fn get_tx_tree(params: Params<'static>, ctx: Arc<Context>) -> RpcResponse<TransactionTree> {
    tracing::info!("JSON-RPC: get_tx_tree()");
    RpcResponse::Err(RpcError::NotFound("TODO".to_string()))
}

#[cfg(test)]
mod tests {

    use std::{env::temp_dir, path::PathBuf};

    use jsonrpsee::{
        core::{client::ClientT, params::ArrayParams},
        http_client::HttpClientBuilder,
    };
    use libsecp256k1::{PublicKey, SecretKey};
    use tracing_subscriber::{filter::LevelFilter, fmt::format::FmtSpan, EnvFilter};

    use crate::mempool;

    use super::*;

    struct AlwaysGrantAclWhitelist;
    #[async_trait::async_trait]
    impl mempool::AclWhitelist for AlwaysGrantAclWhitelist {
        async fn contains(&self, key: &PublicKey) -> Result<bool> {
            Ok(true)
        }
    }

    #[ignore]
    #[tokio::test]
    async fn test_send_transaction() {
        start_logger(LevelFilter::INFO);
        let rpc_server = new_rpc_server().await;

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

        dbg!(resp);
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

    async fn new_rpc_server() -> RpcServer {
        let cfg = Arc::new(Config {
            data_directory: temp_dir(),
            db_url: "postgres://gevulot:gevulot@localhost/gevulot".to_string(),
            json_rpc_listen_addr: "127.0.0.1:0".parse().unwrap(),
            log_directory: temp_dir(),
            node_key_file: PathBuf::new().join("node.key"),
            p2p_discovery_addrs: vec![],
            p2p_listen_addr: "127.0.0.1:9999".parse().unwrap(),
            p2p_psk_passphrase: "secret.".to_string(),
            provider: "qemu".to_string(),
            vsock_listen_port: 8080,
            num_cpus: 8,
            mem_gb: 8,
            gpu_devices: None,
            http_download_port: 0,
        });

        let db = Arc::new(Database::new(&cfg.db_url).await.unwrap());
        let mempool = Arc::new(RwLock::new(
            Mempool::new(db.clone(), Arc::new(AlwaysGrantAclWhitelist {}), None)
                .await
                .unwrap(),
        ));
        let asset_manager = Arc::new(AssetManager::new(
            cfg.clone(),
            db.clone(),
            Arc::new(RwLock::new(std::collections::HashMap::new())),
        ));

        RpcServer::run(cfg.clone(), db.clone(), mempool, asset_manager)
            .await
            .expect("rpc_server.run")
    }
}
