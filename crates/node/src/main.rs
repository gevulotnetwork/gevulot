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

use crate::networking::WhitelistSyncer;
use crate::txvalidation::{CallbackSender, ValidatedTxReceiver};

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

    //To show to idea. Should use your config definition
    let new_validated_tx_receiver: Arc<RwLock<dyn ValidatedTxReceiver>> = if !config.no_execution {
        let mempool = Arc::new(RwLock::new(Mempool::new(database.clone()).await?));

        let scheduler = scheduler::start_scheduler(
            config.clone(),
            database.clone(),
            mempool.clone(),
            node_key,
            tx_sender.clone(),
        )
        .await;

        // Run Scheduler in its own task.
        tokio::spawn(async move { scheduler.run().await });
        mempool
    } else {
        struct ArchiveMempool(Arc<Database>);
        #[async_trait]
        impl ValidatedTxReceiver for ArchiveMempool {
            async fn send_new_tx(&mut self, tx: Transaction<Validated>) -> eyre::Result<()> {
                self.0.as_ref().add_transaction(&tx).await
            }
        }
        Arc::new(RwLock::new(ArchiveMempool(database.clone())))
    };

    // Start Tx process event loop.
    let (txevent_loop_jh, p2p_stream) = txvalidation::spawn_event_loop(
        config.data_directory.clone(),
        config.p2p_listen_addr,
        config.http_download_port,
        http_peer_list.clone(),
        database.clone(),
        rcv_tx_event_rx,
        new_validated_tx_receiver.clone(),
    )
    .await?;

    let public_node_key = PublicKey::from_secret_key(&node_key);
    let p2p = Arc::new(
        networking::P2P::new(
            "gevulot-p2p-network",
            config.p2p_listen_addr,
            &config.p2p_psk_passphrase,
            public_node_key,
            Some(config.http_download_port),
            config.p2p_advertised_listen_addr,
            http_peer_list,
            txvalidation::TxEventSender::<txvalidation::P2pSender>::build(tx_sender.clone()),
            p2p_stream,
        )
        .await,
    );

    let p2p_listen_addr = p2p.node().start_listening().await?;
    tracing::info!("listening for p2p at {}", p2p_listen_addr);

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
            PublicKey::from_secret_key(&SecretKey::default()), // P2P beacons don't need the key atm.
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
