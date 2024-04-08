use std::{collections::BTreeSet, net::SocketAddr};

use gevulot_node::types::{self, transaction::Validated};
use libsecp256k1::PublicKey;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct Handshake {
    pub my_p2p_listen_addr: SocketAddr,
    pub peers: BTreeSet<SocketAddr>,
    pub http_port: Option<u16>,
}

#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) enum Message {
    Transaction(types::Transaction<Validated>),
    DiagnosticsRequest(DiagnosticsRequestKind),
    DiagnosticsResponse(PublicKey, DiagnosticsResponse),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) enum DiagnosticsRequestKind {
    Version,
    Resources,
    Metrics,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) enum DiagnosticsResponse {
    Version {
        major: u16,
        minor: u16,
        patch: u16,
        build: String,
    },
    Resources {
        cpus: u64,
        mem: u64,
        gpus: u64,
    },
    Metrics(Vec<u8>),
}
