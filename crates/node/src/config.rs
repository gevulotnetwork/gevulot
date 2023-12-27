use clap::Parser;
use std::{net::SocketAddr, path::PathBuf};

#[derive(Parser)]
#[command(author, version, about = "Gevulot node")]
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
}
