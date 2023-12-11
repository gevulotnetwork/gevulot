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
