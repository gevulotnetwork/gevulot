use std::{net::SocketAddr, path::PathBuf};

use clap::{Args, Parser, Subcommand};

#[derive(Debug, Args)]
pub struct Config {
    #[arg(
        long,
        long_help = "Peer whitelist URL",
        env = "GEVULOT_ACL_WHITELIST_URL"
    )]
    pub acl_whitelist_url: Option<String>,

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
        long_help = "No execution flag. When set as true, the node does not execute transactions.",
        env = "GEVULOT_NODE_NO_EXECUTION",
        default_value_t = false
    )]
    pub no_execution: bool,

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
        value_delimiter = ',',
        env = "GEVULOT_P2P_DISCOVERY_ADDR"
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
        long_help = "Advertised P2P listen address (if different from the effective P2P listen address)",
        env = "GEVULOT_P2P_ADVERTISED_LISTEN_ADDR"
    )]
    pub p2p_advertised_listen_addr: Option<SocketAddr>,

    #[arg(
        long,
        long_help = "Port open to download transaction data between nodes. Use P2P interface to bind.",
        env = "GEVULOT_HTTP_PORT",
        default_value = "9995"
    )]
    pub http_download_port: u16,

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

    #[arg(long, long_help = "Number of CPUs available", env = "GEVULOT_CPUS")]
    pub num_cpus: Option<u64>,

    #[arg(
        long,
        long_help = "Amount of memory available (in GBs)",
        env = "GEVULOT_MEM_GB"
    )]
    pub mem_gb: Option<u64>,

    #[arg(long, long_help = "GPU PCI devices", env = "GEVULOT_GPU_DEVICES")]
    pub gpu_devices: Option<String>,

    #[arg(
        long,
        long_help = "Healthcheck listen address",
        env = "GEVULOT_HEALTHCHECK_LISTEN_ADDR",
        default_value = "127.0.0.1:8888"
    )]
    pub http_healthcheck_listen_addr: SocketAddr,
}

#[derive(Debug, Args)]
pub struct KeyOptions {
    #[arg(
        long,
        long_help = "Key filename",
        default_value_os_t = PathBuf::from("/var/lib/gevulot/node.key"),
    )]
    pub key_file: PathBuf,
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

#[derive(Debug, Args)]
pub struct P2PBeaconConfig {
    #[arg(
        long,
        long_help = "Directory where the node should store its data",
        env = "GEVULOT_DATA_DIRECTORY", 
        default_value_os_t = PathBuf::from("/var/lib/gevulot"),
    )]
    pub data_directory: PathBuf,
    #[arg(
        long,
        long_help = "P2P listen address",
        env = "GEVULOT_P2P_LISTEN_ADDR",
        default_value = "127.0.0.1:9999"
    )]
    pub p2p_listen_addr: SocketAddr,

    #[arg(
        long,
        long_help = "Advertised P2P listen address (if different from the effective P2P listen address)",
        env = "GEVULOT_P2P_ADVERTISED_LISTEN_ADDR"
    )]
    pub p2p_advertised_listen_addr: Option<SocketAddr>,

    #[arg(
        long,
        long_help = "P2P PSK passphrase",
        env = "GEVULOT_PSK_PASSPHRASE",
        default_value = "Pack my box with five dozen liquor jugs."
    )]
    pub p2p_psk_passphrase: String,

    #[arg(
        long,
        long_help = "HTTP port for downloading transaction data between nodes. Uses same interface as P2P listen address.",
        env = "GEVULOT_HTTP_PORT",
        default_value = "9995"
    )]
    pub http_download_port: u16,
}

#[derive(Debug, Subcommand)]
pub enum GenerateCommand {
    Key {
        #[command(flatten)]
        options: KeyOptions,
    },
    // NOTE: Depracated. Will be eventually removed. Use `Key` instead.
    NodeKey {
        #[command(flatten)]
        options: KeyOptions,
    },
}

#[derive(Debug, Subcommand)]
pub enum ShowCommand {
    PublicKey {
        #[arg(
        long,
        long_help = "Key filename",
        default_value_os_t = PathBuf::from("/var/lib/gevulot/node.key"),
        )]
        key_file: PathBuf,
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

    /// P2PBeacon is a run mode where only P2P code is executed. Used for coordinating other nodes in the network.
    P2PBeacon {
        #[command(flatten)]
        config: P2PBeaconConfig,
    },

    /// Run the node.
    Run {
        #[command(flatten)]
        config: Config,
    },

    /// Show information.
    Show {
        #[command(subcommand)]
        op: ShowCommand,
    },
}

#[derive(Debug, Parser)]
#[command(author, version, about = "Gevulot node")]
pub struct Cli {
    #[command(subcommand)]
    pub subcommand: Command,
}
