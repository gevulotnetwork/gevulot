#![allow(dead_code)]
#![allow(unused_variables)]

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
use std::collections::HashMap;
use std::net::SocketAddr;
use std::{
    io::{ErrorKind, Write},
    net::ToSocketAddrs,
    path::PathBuf,
    sync::{Arc, Mutex},
    thread::sleep,
    time::Duration,
};
use tokio::sync::mpsc;
use tokio::sync::{Mutex as TMutex, RwLock};
use tokio_stream::wrappers::UnboundedReceiverStream;
use tonic::transport::Server;
use tracing_subscriber::{filter::LevelFilter, fmt::format::FmtSpan, EnvFilter};
use types::{transaction::Validated, Hash, Transaction};
use workflow::WorkflowEngine;

mod cli;
mod mempool;
mod nanos;
mod networking;
mod rpc_server;
mod scheduler;
mod storage;
mod txvalidation;
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
        .with_target(true)
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
    async fn get(&self, hash: &Hash) -> Result<Option<Transaction<Validated>>> {
        self.find_transaction(hash).await
    }

    async fn set(&self, tx: &Transaction<Validated>) -> Result<()> {
        let tx_hash = tx.hash;
        self.add_transaction(tx).await?;
        self.add_asset(&tx_hash).await?;
        self.mark_asset_complete(&tx_hash).await
    }

    async fn fill_deque(
        &self,
        deque: &mut std::collections::VecDeque<Transaction<Validated>>,
    ) -> Result<()> {
        for t in self.get_unexecuted_transactions().await? {
            deque.push_back(t);
        }

        Ok(())
    }
}

#[async_trait]
impl workflow::TransactionStore for storage::Database {
    async fn find_transaction(&self, tx_hash: &Hash) -> Result<Option<Transaction<Validated>>> {
        self.find_transaction(tx_hash).await
    }
}

async fn run(config: Arc<Config>) -> Result<()> {
    let database = Arc::new(Database::new(&config.db_url).await?);

    let http_peer_list: Arc<tokio::sync::RwLock<HashMap<SocketAddr, Option<u16>>>> =
        Default::default();

    let mempool = Arc::new(RwLock::new(Mempool::new(database.clone()).await?));

    // Start Tx process event loop.
    let (txevent_loop_jh, tx_sender, p2p_stream) = txvalidation::spawn_event_loop(
        config.data_directory.clone(),
        config.p2p_listen_addr,
        config.http_download_port,
        http_peer_list.clone(),
        database.clone(),
        mempool.clone(),
    )
    .await?;

    let p2p = Arc::new(
        networking::P2P::new(
            "gevulot-p2p-network",
            config.p2p_listen_addr,
            &config.p2p_psk_passphrase,
            Some(config.http_download_port),
            config.p2p_advertised_listen_addr,
            http_peer_list,
            txvalidation::TxEventSender::<txvalidation::P2pSender>::build(tx_sender.clone()),
            p2p_stream,
        )
        .await,
    );

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

    let node_key = read_node_key(&config.node_key_file)?;

    let workflow_engine = Arc::new(WorkflowEngine::new(database.clone()));
    let download_url_prefix = format!(
        "http://{}:{}",
        config.p2p_listen_addr.ip(),
        config.http_download_port
    );

    let scheduler = Arc::new(scheduler::Scheduler::new(
        mempool.clone(),
        database.clone(),
        program_manager,
        workflow_engine,
        node_key,
        config.data_directory.clone(),
        download_url_prefix,
        txvalidation::TxEventSender::<txvalidation::TxResultSender>::build(tx_sender.clone()),
    ));

    let vm_server =
        vmm::vm_server::VMServer::new(scheduler.clone(), provider, config.data_directory.clone());

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
        txvalidation::TxEventSender::<txvalidation::RpcSender>::build(tx_sender),
    )
    .await?;

    if let Err(err) = txevent_loop_jh.await {
        tracing::info!("Tx event loop error:{err}");
    }
    Ok(())
}

/// p2p_beacon brings up P2P networking but nothing else. This function can be
/// used for independent P2P network beacon that provides connectivity for
/// others, while it doesn't participate in the Gevulot's operational side
/// in any other way - i.e. this won't handle transactions in any way.
async fn p2p_beacon(config: P2PBeaconConfig) -> Result<()> {
    let http_peer_list: Arc<tokio::sync::RwLock<HashMap<SocketAddr, Option<u16>>>> =
        Default::default();

    // Build an empty channel for P2P interface's `Transaction` management.
    // Indicate some domain conflict issue.
    // P2P network should be started (peer domain) without Tx management (Node domain).
    let (tx, mut rcv_tx_event_rx) = mpsc::unbounded_channel();
    tokio::spawn(async move { while rcv_tx_event_rx.recv().await.is_some() {} });

    let (_, p2p_recv) = mpsc::unbounded_channel::<Transaction<Validated>>();
    let p2p_stream = UnboundedReceiverStream::new(p2p_recv);

    let p2p = Arc::new(
        networking::P2P::new(
            "gevulot-network",
            config.p2p_listen_addr,
            &config.p2p_psk_passphrase,
            None,
            config.p2p_advertised_listen_addr,
            http_peer_list,
            txvalidation::TxEventSender::<txvalidation::P2pSender>::build(tx),
            p2p_stream,
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
