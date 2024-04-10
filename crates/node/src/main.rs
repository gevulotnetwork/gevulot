#![allow(dead_code)]
#![allow(unused_variables)]

use async_trait::async_trait;
use clap::Parser;
use cli::{
    Cli, Command, Config, GenerateCommand, KeyOptions, P2PBeaconConfig, PeerCommand, ShowCommand,
};
use eyre::Result;
use gevulot_node::types;
use gevulot_node::types::transaction::Received;
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
    sync::Arc,
    thread::sleep,
    time::Duration,
};
use tokio::sync::mpsc;
use tokio::sync::RwLock;
use tokio_stream::wrappers::UnboundedReceiverStream;
use tracing_subscriber::{filter::LevelFilter, fmt::format::FmtSpan, EnvFilter};
use types::{transaction::Validated, Hash, Transaction};

use http_body_util::Full;
use hyper::body::Bytes;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use std::convert::Infallible;
use tokio::net::TcpListener;

mod cli;
mod mempool;
mod metrics;
mod networking;
mod rpc_server;
mod scheduler;
mod storage;
mod vmm;
mod watchdog;
mod workflow;

use mempool::Mempool;
use storage::{database::entity, Database};

use crate::mempool::CallbackSender;
use crate::networking::WhitelistSyncer;

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
            GenerateCommand::NodeKey { options } => {
                eprintln!("WARNING: `node-key` command is deprecated. Please use `key` instead.");
                generate_key(options)
            }
            GenerateCommand::Key { options } => generate_key(options),
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
                db.acl_whitelist(key).await
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

fn generate_key(opts: KeyOptions) -> Result<()> {
    let key = SecretKey::random(&mut StdRng::from_entropy());
    let public_key = PublicKey::from_secret_key(&key);
    let mut fd = match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&opts.key_file)
    {
        Ok(fd) => fd,
        Err(err) => match err.kind() {
            ErrorKind::NotFound => {
                eprintln!("directory for {:#?} doesn't exist", &opts.key_file);
                std::process::exit(1);
            }
            ErrorKind::AlreadyExists => {
                eprintln!("file {:#?} already exists", &opts.key_file);
                std::process::exit(1);
            }
            _ => return Err(err.into()),
        },
    };

    fd.write_all(&key.serialize()[..])?;
    fd.flush()?;

    println!(
        "Key generated and saved in file {}\nPublic key: {}",
        &opts.key_file.to_str().unwrap_or(""),
        hex::encode(public_key.serialize()),
    );

    Ok(())
}

#[async_trait]
impl mempool::ValidateStorage for storage::Database {
    async fn get_tx(&self, hash: &Hash) -> Result<Option<Transaction<Validated>>> {
        self.find_transaction(hash).await
    }

    async fn contains_program(&self, hash: Hash) -> Result<bool> {
        self.find_program(hash).await.map(|res| res.is_some())
    }
}

#[async_trait]
impl mempool::Storage for storage::Database {
    async fn get(&self, hash: &Hash) -> Result<Option<Transaction<Validated>>> {
        self.find_transaction(hash).await
    }

    async fn set(&self, tx: &Transaction<Validated>) -> Result<()> {
        let tx_hash = tx.hash;
        self.add_transaction(tx).await
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

impl crate::mempool::MempoolStorage for storage::Database {}

#[async_trait]
impl workflow::TransactionStore for storage::Database {
    async fn find_transaction(&self, tx_hash: &Hash) -> Result<Option<Transaction<Validated>>> {
        self.find_transaction(tx_hash).await
    }
    async fn mark_tx_executed(&self, tx_hash: &Hash) -> Result<()> {
        self.mark_tx_executed(tx_hash).await
    }
}

async fn run(config: Arc<Config>) -> Result<()> {
    let node_key = read_node_key(&config.node_key_file)?;

    // Register metrics counters.
    metrics::register_metrics();

    if let Some(http_metrics_bind_addr) = config.http_metrics_listen_addr {
        // Start HTTP metrics server.
        metrics::serve_metrics(http_metrics_bind_addr).await?;
        tracing::info!("listening for metrics at {http_metrics_bind_addr}");
    }

    let database = Arc::new(Database::new(&config.db_url).await?);

    // Launch the ACL whitelist syncing early in the startup.
    if let Some(ref whitelist_url) = config.acl_whitelist_url {
        let acl_whitelist_syncer = WhitelistSyncer::new(whitelist_url.clone(), database.clone());
        tokio::spawn(async move {
            loop {
                if let Err(err) = acl_whitelist_syncer.sync().await {
                    tracing::warn!("failed to sync ACL whitelist: {}", err);
                }

                // Sync the whitelist every 10min.
                sleep(Duration::from_secs(600));
            }
        });
    }

    let http_peer_list: Arc<tokio::sync::RwLock<HashMap<SocketAddr, Option<u16>>>> =
        Default::default();

    // Define the channel that receives transactions from the outside of the node.
    // These transactions must be validated first.
    let (tx_sender, rcv_tx_event_rx) =
        mpsc::unbounded_channel::<(Transaction<Received>, Option<CallbackSender>)>();

    //Start Mempool
    let mempool = Arc::new(RwLock::new(Mempool::new(database.clone()).await?));
    let (txevent_loop_jh, p2p_stream) = mempool::Mempool::start_tx_validation_event_loop(
        config.data_directory.clone(),
        config.p2p_listen_addr,
        config.http_download_port,
        http_peer_list.clone(),
        rcv_tx_event_rx,
        mempool.clone(),
        database.clone(),
        database.clone(),
    )
    .await?;

    if !config.no_execution {
        //start execution scheduler.
        let scheduler_watchdog_sender =
            watchdog::start_healthcheck(config.http_healthcheck_listen_addr).await?;
        let scheduler = scheduler::start_scheduler(
            config.clone(),
            database.clone(),
            mempool.clone(),
            node_key,
            tx_sender.clone(),
        )
        .await;

        // Run Scheduler in its own task.
        tokio::spawn(async move { scheduler.run(scheduler_watchdog_sender).await });
    }

    let public_node_key = PublicKey::from_secret_key(&node_key);
    let node_resources = scheduler::get_configured_resources(&config);
    let p2p = Arc::new(
        networking::P2P::new(
            "gevulot-p2p-network",
            config.p2p_listen_addr,
            &config.p2p_psk_passphrase,
            public_node_key,
            Some(config.http_download_port),
            config.p2p_advertised_listen_addr,
            http_peer_list,
            mempool::TxEventSender::<mempool::P2pSender>::build(tx_sender.clone()),
            p2p_stream,
            node_resources,
        )
        .await,
    );

    let p2p_listen_addr = p2p.node().start_listening().await?;
    tracing::info!("listening for p2p at {}", p2p_listen_addr);

    let mut connected_nodes = 0;
    for addr in config.p2p_discovery_addrs.clone() {
        tracing::info!("connecting to p2p peer {}", addr);
        match addr.to_socket_addrs() {
            Ok(mut socket_iter) => {
                if let Some(peer) = socket_iter.next() {
                    let (connected, fail) = p2p.connect(peer).await;
                    connected_nodes += connected.len();
                    if !fail.is_empty() {
                        tracing::info!("Peer connection, fail to connect to these peers:{fail:?}");
                    }
                }
            }
            Err(err) => {
                tracing::error!("failed to resolve {}: {}", addr, err);
            }
        }
    }

    // If we couldn't connect to any P2P nodes, we'll be left out alone forever.
    // Useless to run at that point.
    if !config.p2p_discovery_addrs.is_empty() && connected_nodes == 0 {
        tracing::error!("Failed to connect to any P2P node. Quitting.");
        std::process::exit(1);
    }

    // Start JSON-RPC server.
    let rpc_server = rpc_server::RpcServer::run(
        config.clone(),
        database.clone(),
        mempool::TxEventSender::<mempool::RpcSender>::build(tx_sender),
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
            PublicKey::from_secret_key(&SecretKey::default()), // P2P beacons don't need the key atm.
            None,
            config.p2p_advertised_listen_addr,
            http_peer_list,
            mempool::TxEventSender::<mempool::P2pSender>::build(tx),
            p2p_stream,
            (0, 0, 0), // P2P beacon node's resources aren't really important.
        )
        .await,
    );

    let p2p_addr = p2p.node().start_listening().await?;
    tracing::info!("listening for p2p at {}", p2p_addr);

    let mut connected_nodes = 0;
    let mut try_count = 0;

    while connected_nodes == 0 && try_count < config.cluster_join_attempt_limit {
        for addr in config.p2p_discovery_addrs.clone() {
            tracing::info!("connecting to p2p peer {}", addr);
            match addr.to_socket_addrs() {
                Ok(mut socket_iter) => {
                    if let Some(peer) = socket_iter.next() {
                        let (connected, fail) = p2p.do_connect(peer, true).await;
                        connected_nodes += connected.len();
                        if !fail.is_empty() {
                            tracing::info!(
                                "Peer connection, fail to connect to these peers:{fail:?}"
                            );
                        }
                    }
                }
                Err(err) => {
                    tracing::error!("failed to resolve {}: {}", addr, err);
                }
            }

            tokio::time::sleep(Duration::from_secs(1)).await;
        }

        try_count += 1;
    }

    if !config.p2p_discovery_addrs.is_empty() && connected_nodes == 0 {
        tracing::info!("No discovery addresses configured or none could be resolved. Assuming we're the first node.");
    }

    // Start a basic healthcheck so kubernetes has something to wait on.
    let listener = TcpListener::bind(config.http_healthcheck_listen_addr).await?;
    tracing::info!("Healthcheck listening on: {}", listener.local_addr()?);

    loop {
        let (stream, _) = listener.accept().await?;

        let io = TokioIo::new(stream);

        tokio::task::spawn(async move {
            if let Err(err) = http1::Builder::new()
                .serve_connection(io, service_fn(ok))
                .await
            {
                tracing::error!("Error serving connection: {:?}", err);
            }
        });
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

async fn ok(_: Request<hyper::body::Incoming>) -> Result<Response<Full<Bytes>>, Infallible> {
    Ok(Response::new(Full::new(Bytes::from("OK"))))
}
