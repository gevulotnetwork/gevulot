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
    V1(MessageV1),
}

#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) enum MessageV1 {
    Transaction(types::Transaction<types::transaction::Validated>),
    DiagnosticsRequest(DiagnosticsRequestKind),
    DiagnosticsResponse(PublicKey, DiagnosticsResponseV1),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) enum DiagnosticsRequestKind {
    Version,
    Resources,
    Metrics,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) enum DiagnosticsResponseV1 {
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

impl From<internal::Message> for Message {
    fn from(value: internal::Message) -> Self {
        match value {
            internal::Message::Transaction(tx) => Message::V1(MessageV1::Transaction(tx)),
            internal::Message::DiagnosticsRequest(kind) => match kind {
                internal::DiagnosticsRequestKind::Version => Message::V1(
                    MessageV1::DiagnosticsRequest(DiagnosticsRequestKind::Version),
                ),
                internal::DiagnosticsRequestKind::Resources => Message::V1(
                    MessageV1::DiagnosticsRequest(DiagnosticsRequestKind::Resources),
                ),
                internal::DiagnosticsRequestKind::Metrics => Message::V1(
                    MessageV1::DiagnosticsRequest(DiagnosticsRequestKind::Metrics),
                ),
            },
            internal::Message::DiagnosticsResponse(pubkey, response) => match response {
                internal::DiagnosticsResponse::Version {
                    major,
                    minor,
                    patch,
                    build,
                } => Message::V1(MessageV1::DiagnosticsResponse(
                    pubkey,
                    DiagnosticsResponseV1::Version {
                        major,
                        minor,
                        patch,
                        build,
                    },
                )),
                internal::DiagnosticsResponse::Resources { cpus, mem, gpus } => {
                    Message::V1(MessageV1::DiagnosticsResponse(
                        pubkey,
                        DiagnosticsResponseV1::Resources { cpus, mem, gpus },
                    ))
                }
                internal::DiagnosticsResponse::Metrics(data) => Message::V1(
                    MessageV1::DiagnosticsResponse(pubkey, DiagnosticsResponseV1::Metrics(data)),
                ),
            },
        }
    }
}

impl Message {
    pub fn parse(bs: &[u8]) -> Result<internal::Message> {
        match bincode::deserialize(bs) {
            Ok(Message::V1(msg)) => match msg {
                MessageV1::Transaction(tx) => Ok(internal::Message::Transaction(tx)),
                MessageV1::DiagnosticsRequest(kind) => match kind {
                    DiagnosticsRequestKind::Version => Ok(internal::Message::DiagnosticsRequest(
                        internal::DiagnosticsRequestKind::Version,
                    )),
                    DiagnosticsRequestKind::Resources => Ok(internal::Message::DiagnosticsRequest(
                        internal::DiagnosticsRequestKind::Resources,
                    )),
                    DiagnosticsRequestKind::Metrics => Ok(internal::Message::DiagnosticsRequest(
                        internal::DiagnosticsRequestKind::Metrics,
                    )),
                },

                MessageV1::DiagnosticsResponse(pubkey, kind) => match kind {
                    DiagnosticsResponseV1::Version {
                        major,
                        minor,
                        patch,
                        build,
                    } => Ok(internal::Message::DiagnosticsResponse(
                        pubkey,
                        internal::DiagnosticsResponse::Version {
                            major,
                            minor,
                            patch,
                            build,
                        },
                    )),
                    DiagnosticsResponseV1::Resources { cpus, mem, gpus } => {
                        Ok(internal::Message::DiagnosticsResponse(
                            pubkey,
                            internal::DiagnosticsResponse::Resources { cpus, mem, gpus },
                        ))
                    }
                    DiagnosticsResponseV1::Metrics(data) => {
                        Ok(internal::Message::DiagnosticsResponse(
                            pubkey,
                            internal::DiagnosticsResponse::Metrics(data),
                        ))
                    }
                },
            },
            Err(err) => Err(err.into()),
        }
    }
}
