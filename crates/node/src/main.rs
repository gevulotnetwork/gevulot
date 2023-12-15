#![allow(dead_code)]
#![allow(unused_variables)]

use asset_manager::AssetManager;
use config::Config;
use gevulot_node::types;

use actix_web::{web, App, HttpServer};
use async_trait::async_trait;
use clap::Parser;
use eyre::Result;
use std::sync::{Arc, Mutex};
use tokio::sync::Mutex as TMutex;
use tokio::sync::RwLock;
use tonic::transport::Server;
use tracing_subscriber::{filter::LevelFilter, fmt::format::FmtSpan, EnvFilter};
use types::{Hash, Transaction};

mod asset_manager;
mod config;
mod mempool;
mod networking;
mod rest_api;
mod rpc_server;
mod scheduler;
mod storage;
mod vmm;

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
        self.add_transaction(tx).await?;
        Ok(())
    }

    async fn fill_deque(&self, deque: &mut std::collections::VecDeque<Transaction>) -> Result<()> {
        for t in self.get_transactions().await? {
            deque.push_back(t);
        }

        Ok(())
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

    // Launch AssetManager's background processing.
    tokio::spawn({
        let asset_mgr = asset_mgr.clone();
        async move { asset_mgr.run().await }
    });

    let scheduler = Arc::new(scheduler::Scheduler::new(mempool.clone(), program_manager));
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

    {
        let app_data = web::Data::new(rest_api::AppState {
            asset_manager: asset_mgr.clone(),
            database: database.clone(),
            file_storage,
            mempool: mempool.clone(),
        });
        HttpServer::new(move || {
            App::new()
                .app_data(app_data.clone())
                .service(rest_api::index)
                .service(rest_api::tasks)
                .service(rest_api::add_task)
                .service(rest_api::programs)
                .service(rest_api::deploy_program)
        })
        .bind(("127.0.0.1", 8080))?
        .run()
        .await?;
    }

    Ok(())
}
