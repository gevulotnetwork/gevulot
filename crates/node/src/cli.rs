use std::{net::SocketAddr, path::PathBuf};

use clap::{Args, Parser, Subcommand};

#[derive(Debug, Args)]
pub struct Config {
    #[arg(
        long,
        long_help = "Directory where the node should store its data",
        env = "GEVULOT_DATA_DIRECTORY", 
        default_value_os_t = PathBuf::from("/var/lib/gevulot"),
    )]
    pub data_directory: PathBuf,

    #[arg(
        long,
        long_help = "Database URL",
        env = "GEVULOT_DB_URL",
        default_value = "postgres://gevulot:gevulot@localhost/gevulot"
    )]
    pub db_url: String,

    #[arg(
        long,
        long_help = "JSON-RPC listen address",
        env = "GEVULOT_JSON_RPC_LISTEN_ADDR",
        default_value = "127.0.0.1:9944"
    )]
    pub json_rpc_listen_addr: SocketAddr,

    #[arg(
        long,
        long_help = "Directory where the node should store logs",
        env = "GEVULOT_LOG_DIRECTORY", 
        default_value_os_t = PathBuf::from("/var/lib/gevulot/log"),
    )]
    pub log_directory: PathBuf,

    #[arg(
        long,
        long_help = "File where the node key is persisted",
        env = "GEVULOT_NODE_KEY_FILE",
        default_value_os_t = PathBuf::from("/var/lib/gevulot/node.key"),
    )]
    pub node_key_file: PathBuf,

    #[arg(
        long,
        long_help = "",
        env = "GEVULOT_P2P_DISCOVERY_ADDR",
        default_value = "bootstrap.p2p.devnet.gevulot.com"
    )]
    pub p2p_discovery_addrs: Vec<String>,

    #[arg(
        long,
        long_help = "P2P listen address",
        env = "GEVULOT_P2P_LISTEN_ADDR",
        default_value = "127.0.0.1:9999"
    )]
    pub p2p_listen_addr: SocketAddr,

    #[arg(
        long,
        long_help = "P2P PSK passphrase",
        env = "GEVULOT_PSK_PASSPHRASE",
        default_value = "Pack my box with five dozen liquor jugs."
    )]
    pub p2p_psk_passphrase: String,

    #[arg(
        long,
        long_help = "Provider backend to run tasks",
        env = "GEVULOT_PROVIDER",
        default_value = "qemu"
    )]
    pub provider: String,

    #[arg(
        long,
        long_help = "Listen port for VSOCK gRPC service",
        env = "GEVULOT_VSOCK_LISTEN_PORT",
        default_value_t = 8080
    )]
    pub vsock_listen_port: u32,

    #[arg(
        long,
        long_help = "Number of CPUs available",
        env = "GEVULOT_CPUS",
        default_value_t = 8
    )]
    pub num_cpus: u64,

    #[arg(
        long,
        long_help = "Amount of memory available (in GBs)",
        env = "GEVULOT_MEM_GB",
        default_value_t = 8
    )]
    pub mem_gb: u64,

    #[arg(long, long_help = "GPU PCI devices", env = "GEVULOT_GPU_DEVICES")]
    pub gpu_devices: Option<String>,
}

#[derive(Debug, Args)]
pub struct NodeKeyOptions {
    #[arg(
        long,
        long_help = "Node key filename",
        default_value_os_t = PathBuf::from("/var/lib/gevulot/node.key"),
    )]
    pub node_key_file: PathBuf,
}

#[derive(Debug, Subcommand)]
pub enum PeerCommand {
    Whitelist {
        #[arg(
            long,
            long_help = "Database URL",
            env = "GEVULOT_DB_URL",
            default_value = "postgres://gevulot:gevulot@localhost/gevulot"
        )]
        db_url: String,
    },
    Deny {
        #[arg(
            long,
            long_help = "Database URL",
            env = "GEVULOT_DB_URL",
            default_value = "postgres://gevulot:gevulot@localhost/gevulot"
        )]
        db_url: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum GenerateCommand {
    NodeKey {
        #[command(flatten)]
        options: NodeKeyOptions,
    },
}

#[allow(clippy::large_enum_variant)]
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Generate objects.
    Generate {
        #[command(subcommand)]
        target: GenerateCommand,
    },

    /// Migrate DB.
    Migrate {
        #[arg(
            long,
            long_help = "Database URL",
            env = "GEVULOT_DB_URL",
            default_value = "postgres://gevulot:gevulot@localhost/gevulot"
        )]
        db_url: String,
    },

    /// Peer related commands.
    Peer {
        peer: String,

        #[command(subcommand)]
        op: PeerCommand,
    },

    /// Run the node.
    Run {
        #[command(flatten)]
        config: Config,
    },
}

#[derive(Debug, Parser)]
#[command(author, version, about = "Gevulot node")]
pub struct Cli {
    #[command(subcommand)]
    pub subcommand: Command,
}
