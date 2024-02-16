use std::{collections::BTreeSet, net::SocketAddr};

use gevulot_node::types;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) enum Handshake {
    V1(HandshakeV1),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct HandshakeV1 {
    pub my_p2p_listen_addr: SocketAddr,
    pub peers: BTreeSet<SocketAddr>,
    pub http_port: Option<u16>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) enum Message {
    V0(MessageV0),
}

#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) enum MessageV0 {
    Transaction(types::Transaction<types::transaction::Validated>),
    DiagnosticsRequest(DiagnosticsRequestKind),
    DiagnosticsResponse(DiagnosticsResponseV0),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) enum DiagnosticsRequestKind {
    Version,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) enum DiagnosticsResponseV0 {
    Version {
        major: u16,
        minor: u16,
        patch: u16,
        build: String,
    },
}
