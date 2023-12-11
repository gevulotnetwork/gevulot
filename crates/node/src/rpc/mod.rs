use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
};

use eyre::Result;
use jsonrpsee::{
    server::{RpcModule, Server, ServerHandle},
    types::{Params, ResponsePayload},
    IntoResponse,
};
use tokio::sync::RwLock;

use crate::{
    asset_manager::AssetManager, config::Config, mempool::Mempool, storage::Database,
    types::Transaction,
};

struct Context {
    req_count: Mutex<u64>,
    database: Arc<Database>,
    mempool: Arc<RwLock<Mempool>>,
    asset_manager: Arc<AssetManager>,
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
            req_count: Mutex::new(0),
            database,
            mempool,
            asset_manager,
        });

        module.register_async_method("sendTransaction", send_transaction)?;
        module.register_async_method("getTransaction", get_transaction)?;
        module.register_async_method("getTransactionTree", get_tx_tree)?;

        let local_addr = server.local_addr().unwrap();
        let server_handle = server.start(module);
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

async fn send_transaction(
    params: Params<'static>,
    ctx: Arc<Context>,
) -> ResponsePayload<'static, <u64 as IntoResponse>::Output> {
    let mut c = ctx.req_count.lock().unwrap();
    *c += 1;
    tracing::info!("JSON-RPC: send_transaction()");

    dbg!(&params);

    // Real logic
    let tx: Transaction = match params.one() {
        Ok(tx) => tx,
        Err(e) => {
            tracing::error!("failed to parse transaction: {}", e);
            return c.into_response();
        }
    };

    dbg!(tx);

    c.into_response()
}

async fn get_transaction(
    params: Params<'static>,
    ctx: Arc<Context>,
) -> ResponsePayload<'static, <u64 as IntoResponse>::Output> {
    let mut c = ctx.req_count.lock().unwrap();
    *c += 1;
    tracing::info!("JSON-RPC: get_transaction()");
    c.into_response()
}

async fn get_tx_tree(
    params: Params<'static>,
    ctx: Arc<Context>,
) -> ResponsePayload<'static, <u64 as IntoResponse>::Output> {
    let mut c = ctx.req_count.lock().unwrap();
    *c += 1;
    tracing::info!("JSON-RPC: get_tx_tree()");
    c.into_response()
}

#[cfg(test)]
mod tests {

    use std::env::temp_dir;

    use jsonrpsee::{
        core::{client::ClientT, params::ArrayParams},
        http_client::HttpClientBuilder,
    };
    use tracing_subscriber::{filter::LevelFilter, fmt::format::FmtSpan, EnvFilter};

    use super::*;

    #[ignore]
    #[tokio::test]
    async fn test_send_transaction() {
        start_logger(LevelFilter::INFO);
        let rpc_server = new_rpc_server().await;

        let url = format!("http://{}", rpc_server.addr());
        let rpc_client = HttpClientBuilder::default()
            .build(url)
            .expect("http client");

        let tx = Transaction::default();
        let mut params = ArrayParams::new();
        params.insert(&tx).expect("rpc params");

        let resp = rpc_client
            .request::<u32, ArrayParams>("sendTransaction", params)
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
            provider: "qemu".to_string(),
            vsock_listen_port: 8080,
        });

        let db = Arc::new(Database::new(&cfg.db_url).await.unwrap());
        let mempool = Arc::new(RwLock::new(Mempool::new(db.clone()).await.unwrap()));
        let asset_manager = Arc::new(AssetManager::new(cfg.clone(), db.clone()));

        RpcServer::run(cfg.clone(), db.clone(), mempool, asset_manager)
            .await
            .expect("rpc_server.run")
    }
}
