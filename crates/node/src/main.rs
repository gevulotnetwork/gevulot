#![allow(dead_code)]
#![allow(unused_variables)]

use asset_manager::AssetManager;
use config::Config;
use gevulot_node::types;

use async_trait::async_trait;
use clap::Parser;
use eyre::Result;
use libsecp256k1::SecretKey;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread::sleep;
use std::time::Duration;
use tokio::sync::Mutex as TMutex;
use tokio::sync::RwLock;
use tonic::transport::Server;
use tracing_subscriber::{filter::LevelFilter, fmt::format::FmtSpan, EnvFilter};
use types::{Hash, Transaction};
use workflow::WorkflowEngine;

mod asset_manager;
mod config;
mod mempool;
mod networking;
mod rpc_server;
mod scheduler;
mod storage;
mod vmm;
mod workflow;

use mempool::Mempool;
use storage::Database;

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

    // Comment above & uncomment below for tokio-console.
    //console_subscriber::init();
}

#[tokio::main]
async fn main() -> Result<()> {
    start_logger(LevelFilter::INFO);

    let config = Arc::new(Config::parse());
    run(config).await?;

    Ok(())
}

#[async_trait]
impl mempool::Storage for storage::Database {
    async fn get(&self, hash: &Hash) -> Result<Option<Transaction>> {
        self.find_transaction(hash).await
    }

    async fn set(&self, tx: &Transaction) -> Result<()> {
        self.add_transaction(tx).await
    }

    async fn fill_deque(&self, deque: &mut std::collections::VecDeque<Transaction>) -> Result<()> {
        for t in self.get_transactions().await? {
            deque.push_back(t);
        }

        Ok(())
    }
}

#[async_trait]
impl workflow::TransactionStore for storage::Database {
    async fn find_transaction(&self, tx_hash: &Hash) -> Result<Option<Transaction>> {
        self.find_transaction(tx_hash).await
    }
}

async fn run(config: Arc<Config>) -> Result<()> {
    let database = Arc::new(Database::new(&config.db_url).await?);
    let file_storage = Arc::new(storage::File::new(&config.data_directory));
    let mempool = Arc::new(RwLock::new(Mempool::new(database.clone()).await?));

    // TODO(tuommaki): read total available resources from config / acquire system stats.
    let resource_manager = Arc::new(Mutex::new(scheduler::ResourceManager::new(16384, 8, 0)));

    // TODO(tuommaki): Handle provider from config.
    let qemu_provider = vmm::qemu::Qemu::new(config.clone());
    let vsock_stream = qemu_provider.vm_server_listener().expect("vsock bind");

    let provider = Arc::new(TMutex::new(qemu_provider));
    let program_manager = scheduler::ProgramManager::new(
        database.clone(),
        provider.clone(),
        resource_manager.clone(),
    );

    let asset_mgr = Arc::new(AssetManager::new(config.clone(), database.clone()));

    let node_key = read_node_key(&config.node_key_file)?;

    // Launch AssetManager's background processing.
    tokio::spawn({
        let asset_mgr = asset_mgr.clone();
        async move { asset_mgr.run().await }
    });

    let workflow_engine = Arc::new(WorkflowEngine::new(database.clone(), file_storage.clone()));

    let scheduler = Arc::new(scheduler::Scheduler::new(
        mempool.clone(),
        database.clone(),
        program_manager,
        workflow_engine,
        node_key,
    ));

    let vm_server =
        vmm::vm_server::VMServer::new(scheduler.clone(), provider, file_storage.clone());

    // Start gRPC VSOCK server.
    tokio::spawn(async move {
        Server::builder()
            .add_service(vm_server.grpc_server())
            .serve_with_incoming(vsock_stream)
            .await
    });

    // Start Scheduler.
    tokio::spawn({
        let scheduler = scheduler.clone();
        async move { scheduler.run().await }
    });

    // Start JSON-RPC server.
    let rpc_server = rpc_server::RpcServer::run(
        config.clone(),
        database.clone(),
        mempool.clone(),
        asset_mgr.clone(),
    )
    .await?;

    loop {
        sleep(Duration::from_secs(1));
    }
}

fn read_node_key(node_key_file: &PathBuf) -> Result<SecretKey> {
    let bs = std::fs::read(node_key_file)?;
    SecretKey::parse(bs.as_slice().try_into().expect("invalid node key")).map_err(|e| e.into())
}
