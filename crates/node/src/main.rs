#![allow(dead_code)]
#![allow(unused_variables)]

use std::{
    io::{ErrorKind, Write},
    net::ToSocketAddrs,
    path::PathBuf,
    sync::{Arc, Mutex},
    thread::sleep,
    time::Duration,
};

use asset_manager::AssetManager;
use async_trait::async_trait;
use clap::Parser;
use cli::{
    Cli, Command, Config, GenerateCommand, NodeKeyOptions, P2PBeaconConfig, PeerCommand,
    ShowCommand,
};
use eyre::Result;
use gevulot_node::types;
use libsecp256k1::{PublicKey, SecretKey};
use pea2pea::Pea2Pea;
use rand::{rngs::StdRng, SeedableRng};
use sqlx::postgres::PgPoolOptions;
use tokio::sync::{Mutex as TMutex, RwLock};
use tonic::transport::Server;
use tracing_subscriber::{filter::LevelFilter, fmt::format::FmtSpan, EnvFilter};
use types::{Hash, Transaction};
use workflow::WorkflowEngine;

mod asset_manager;
mod cli;
mod mempool;
mod nanos;
mod networking;
mod rpc_server;
mod scheduler;
mod storage;
mod vmm;
mod workflow;

use mempool::Mempool;
use storage::{database::entity, Database};

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

    let cli = Cli::parse();

    match cli.subcommand {
        Command::Generate { target } => match target {
            GenerateCommand::NodeKey { options } => generate_node_key(options),
        },
        Command::Migrate { db_url } => {
            let pool = PgPoolOptions::new()
                .max_connections(5)
                .acquire_timeout(Duration::from_millis(500))
                .connect(&db_url)
                .await?;
            // This will pick them up from `./migrations`.
            sqlx::migrate!().run(&pool).await.map_err(|e| e.into())
        }
        Command::Peer { peer, op } => match op {
            PeerCommand::Whitelist { db_url } => {
                let db = storage::Database::new(&db_url).await?;
                let key = entity::PublicKey::try_from(peer.as_str())?;
                db.acl_whitelist(&key).await
            }
            PeerCommand::Deny { db_url } => {
                let db = storage::Database::new(&db_url).await?;
                let key = entity::PublicKey::try_from(peer.as_str())?;
                db.acl_deny(&key).await
            }
        },
        Command::P2PBeacon { config } => p2p_beacon(config).await,
        Command::Run { config } => run(Arc::new(config)).await,
        Command::Show { op } => match op {
            ShowCommand::PublicKey { key_file } => {
                let bs = std::fs::read(key_file)?;
                let key = SecretKey::parse(bs.as_slice().try_into()?)?;
                let public_key = PublicKey::from_secret_key(&key);
                println!("{}", hex::encode(public_key.serialize()));
                Ok(())
            }
        },
    }
}

fn generate_node_key(opts: NodeKeyOptions) -> Result<()> {
    let key = SecretKey::random(&mut StdRng::from_entropy());
    let mut fd = match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&opts.node_key_file)
    {
        Ok(fd) => fd,
        Err(err) => match err.kind() {
            ErrorKind::NotFound => {
                eprintln!("directory for {:#?} doesn't exist", &opts.node_key_file);
                std::process::exit(1);
            }
            ErrorKind::AlreadyExists => {
                eprintln!("file {:#?} already exists", &opts.node_key_file);
                std::process::exit(1);
            }
            _ => return Err(err.into()),
        },
    };

    fd.write_all(&key.serialize()[..])?;
    fd.flush()?;
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
        for t in self.get_unexecuted_transactions().await? {
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

struct P2PTxHandler {
    mempool: Arc<RwLock<Mempool>>,
    database: Arc<Database>,
}

impl P2PTxHandler {
    pub fn new(mempool: Arc<RwLock<Mempool>>, database: Arc<Database>) -> Self {
        Self { mempool, database }
    }
}

#[async_trait::async_trait]
impl networking::p2p::TxHandler for P2PTxHandler {
    async fn recv_tx(&self, tx: Transaction) -> Result<()> {
        // The transaction was received from P2P network so we can consider it
        // propagated at this point.
        let tx_hash = tx.hash;
        let mut tx = tx;
        tx.propagated = true;

        // Submit the tx to mempool.
        self.mempool.write().await.add(tx).await?;

        //TODO copy paste of the asset manager handle_transaction method.
        //added because when a tx arrive from the p2p asset are not added.
        //should be done in a better way.
        self.database.add_asset(&tx_hash).await
    }
}

#[async_trait::async_trait]
impl mempool::AclWhitelist for Database {
    async fn contains(&self, key: &PublicKey) -> Result<bool> {
        let key = entity::PublicKey(*key);
        self.acl_whitelist_has(&key).await
    }
}

async fn run(config: Arc<Config>) -> Result<()> {
    let database = Arc::new(Database::new(&config.db_url).await?);
    let file_storage = Arc::new(storage::File::new(&config.data_directory));

    let p2p = Arc::new(
        networking::P2P::new(
            "gevulot-p2p-network",
            config.p2p_listen_addr,
            &config.p2p_psk_passphrase,
            Some(config.http_download_port),
            config.p2p_advertised_listen_addr,
        )
        .await,
    );

    let mempool = Arc::new(RwLock::new(
        Mempool::new(database.clone(), database.clone(), Some(p2p.clone())).await?,
    ));

    p2p.register_tx_handler(Arc::new(P2PTxHandler::new(
        mempool.clone(),
        database.clone(),
    )))
    .await;

    //start http download manager
    let download_jh = networking::download_manager::serve_files(&config).await?;

    // TODO(tuommaki): read total available resources from config / acquire system stats.
    let num_gpus = if config.gpu_devices.is_some() { 1 } else { 0 };
    let resource_manager = Arc::new(Mutex::new(scheduler::ResourceManager::new(
        config.mem_gb * 1024 * 1024 * 1024,
        config.num_cpus,
        num_gpus,
    )));

    // TODO(tuommaki): Handle provider from config.
    let qemu_provider = vmm::qemu::Qemu::new(config.clone());
    let vsock_stream = qemu_provider.vm_server_listener().expect("vsock bind");

    let provider = Arc::new(TMutex::new(qemu_provider));
    let program_manager = scheduler::ProgramManager::new(
        database.clone(),
        provider.clone(),
        resource_manager.clone(),
    );

    let asset_mgr = Arc::new(AssetManager::new(
        config.clone(),
        database.clone(),
        p2p.as_ref().peer_http_port_list.clone(),
    ));

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

    let p2p_addr = p2p.node().start_listening().await?;
    tracing::info!("listening for p2p at {}", p2p_addr);

    for addr in config.p2p_discovery_addrs.clone() {
        tracing::info!("connecting to p2p peer {}", addr);
        match addr.to_socket_addrs() {
            Ok(mut socket_iter) => {
                if let Some(peer) = socket_iter.next() {
                    p2p.node().connect(peer).await?;
                    break;
                }
            }
            Err(err) => {
                tracing::error!("failed to resolve {}: {}", addr, err);
            }
        }
    }

    // Start JSON-RPC server.
    let rpc_server = rpc_server::RpcServer::run(
        config.clone(),
        database.clone(),
        mempool.clone(),
        asset_mgr.clone(),
    )
    .await?;

    if let Err(err) = download_jh.await {
        tracing::info!("download_manager error:{err}");
    }
    Ok(())
}

/// p2p_beacon brings up P2P networking but nothing else. This function can be
/// used for independent P2P network beacon that provides connectivity for
/// others, while it doesn't participate in the Gevulot's operational side
/// in any other way - i.e. this won't handle transactions in any way.
async fn p2p_beacon(config: P2PBeaconConfig) -> Result<()> {
    let p2p = Arc::new(
        networking::P2P::new(
            "gevulot-network",
            config.p2p_listen_addr,
            &config.p2p_psk_passphrase,
            None,
            config.p2p_advertised_listen_addr,
        )
        .await,
    );

    let p2p_addr = p2p.node().start_listening().await?;
    tracing::info!("listening for p2p at {}", p2p_addr);

    loop {
        sleep(Duration::from_secs(1));
    }
}

fn read_node_key(node_key_file: &PathBuf) -> Result<SecretKey> {
    let bs = match std::fs::read(node_key_file) {
        Ok(key_data) => key_data,
        Err(err) => match err.kind() {
            std::io::ErrorKind::NotFound => {
                eprintln!(
                    "\nerror: node key not found.\n\nplease create node-key with:\n\t{} generate node-key\n",
                    std::env::current_exe().unwrap().to_str().unwrap()
                );
                std::process::exit(1);
            }
            _ => return Err(err.into()),
        },
    };

    SecretKey::parse(bs.as_slice().try_into().expect("invalid node key")).map_err(|e| e.into())
}
