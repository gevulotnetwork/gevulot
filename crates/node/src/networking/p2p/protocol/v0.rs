use eyre::Result;
use std::{collections::BTreeSet, net::SocketAddr};

use gevulot_node::types;
use libsecp256k1::PublicKey;
use serde::{Deserialize, Serialize};

use super::internal;

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

impl From<internal::Handshake> for Handshake {
    fn from(value: internal::Handshake) -> Self {
        let internal::Handshake {
            my_p2p_listen_addr,
            peers,
            http_port,
        } = value;
        Handshake::V1(HandshakeV1 {
            my_p2p_listen_addr,
            peers,
            http_port,
        })
    }
}

impl Handshake {
    pub fn parse(bs: &[u8]) -> Result<internal::Handshake> {
        match bincode::deserialize(bs) {
            Ok(Handshake::V1(HandshakeV1 {
                my_p2p_listen_addr,
                peers,
                http_port,
            })) => Ok(internal::Handshake {
                my_p2p_listen_addr,
                peers,
                http_port,
            }),
            Err(err) => Err(err.into()),
        }
    }
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
    DiagnosticsResponse(PublicKey, DiagnosticsResponseV0),
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

impl From<internal::Message> for Message {
    fn from(value: internal::Message) -> Self {
        match value {
            internal::Message::Transaction(tx) => Message::V0(MessageV0::Transaction(tx)),
            internal::Message::DiagnosticsRequest(kind) => Message::V0(
                MessageV0::DiagnosticsRequest(DiagnosticsRequestKind::Version),
            ),
            internal::Message::DiagnosticsResponse(pubkey, response) => match response {
                internal::DiagnosticsResponse::Version {
                    major,
                    minor,
                    patch,
                    build,
                } => Message::V0(MessageV0::DiagnosticsResponse(
                    pubkey,
                    DiagnosticsResponseV0::Version {
                        major,
                        minor,
                        patch,
                        build,
                    },
                )),
                internal::DiagnosticsResponse::Resources { cpus, mem, gpus } => {
                    Message::V0(MessageV0::DiagnosticsResponse(
                        pubkey,
                        DiagnosticsResponseV0::Version {
                            major: 0,
                            minor: 0,
                            patch: 0,
                            build: String::new(),
                        },
                    ))
                }
                internal::DiagnosticsResponse::Metrics(data) => {
                    Message::V0(MessageV0::DiagnosticsResponse(
                        pubkey,
                        DiagnosticsResponseV0::Version {
                            major: 0,
                            minor: 0,
                            patch: 0,
                            build: String::new(),
                        },
                    ))
                }
            },
        }
    }
}

impl Message {
    pub fn parse(bs: &[u8]) -> Result<internal::Message> {
        match bincode::deserialize(bs) {
            Ok(Message::V0(msg)) => match msg {
                MessageV0::Transaction(tx) => Ok(internal::Message::Transaction(tx)),
                MessageV0::DiagnosticsRequest(_) => Ok(internal::Message::DiagnosticsRequest(
                    internal::DiagnosticsRequestKind::Version,
                )),
                MessageV0::DiagnosticsResponse(
                    pubkey,
                    DiagnosticsResponseV0::Version {
                        major,
                        minor,
                        patch,
                        build,
                    },
                ) => Ok(internal::Message::DiagnosticsResponse(
                    pubkey,
                    internal::DiagnosticsResponse::Version {
                        major,
                        minor,
                        patch,
                        build,
                    },
                )),
            },
            Err(err) => Err(err.into()),
        }
    }
}
