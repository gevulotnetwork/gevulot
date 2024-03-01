use crate::types::file::Output;
use crate::types::file::TxFile;
use crate::types::transaction::{Payload, ProgramMetadata, Validated, Workflow};
use crate::types::Hash;
use crate::types::Transaction;
use base64::Engine;
use jsonrpsee::IntoResponse;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::error::Error;
use std::net::SocketAddr;
use thiserror::Error;

#[allow(clippy::enum_variant_names)]
#[derive(Clone, Error, Debug, Serialize, Deserialize)]
pub enum RpcError {
    #[error("not found: {0}")]
    NotFound(String),

    #[error("A tx was missing: {0}")]
    MissingTx(String),

    #[error("invalid request: {0}")]
    InvalidRequest(String),

    #[error("unauthorized")]
    Unauthorized,

    #[error("InternalError: {0}")]
    InternalError(String),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum RpcResponse<T: Clone> {
    Ok(T),
    Err(RpcError),
}

impl<T: Clone> From<RpcResponse<T>> for Result<T, Box<dyn Error>> {
    fn from(reponse: RpcResponse<T>) -> Self {
        match reponse {
            RpcResponse::Ok(v) => Ok(v),
            RpcResponse::Err(rpc_error) => Err(format!("RPC response error:{}", rpc_error).into()),
        }
    }
}

impl<T: Clone + Serialize> IntoResponse for RpcResponse<T> {
    type Output = RpcResponse<T>;

    fn into_response(self) -> jsonrpsee::types::ResponsePayload<'static, Self::Output> {
        jsonrpsee::types::ResponsePayload::Result(Cow::Owned(self))
    }
}

//RPC Serialiazation Structs.

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct TxOutputFile {
    // Url of the file. File can be retrieve using the node HTTP server with the url.
    url: String,
    // Checksum hex encoded of the file for verification
    checksum: String,
    // Path of the file inside the VM. Use to help to recognize it.
    vm_path: String,
}

impl TxOutputFile {
    pub fn from_txfile(
        file: TxFile<Output>,
        tx_hash: Hash,
        scheme: &str,
        host: SocketAddr,
    ) -> Self {
        //use parse to create an URL, no new method.
        let mut url = reqwest::Url::parse(&format!("{}localhost", scheme))
            .unwrap_or(reqwest::Url::parse("http://localhost").unwrap()); //unwrap always succeed
        url.set_ip_host(host.ip()).unwrap(); //unwrap always succeed
        url.set_port(Some(host.port())).unwrap(); //unwrap always succeed
        url.set_path(&file.clone().into_download_file(tx_hash).get_uri());

        TxOutputFile {
            url: url.to_string(),
            checksum: file.checksum.to_string(),
            vm_path: file.name,
        }
    }
}

#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub enum TxRpcPayload {
    Empty,
    Transfer {
        to: String,
        value: String,
    },
    Stake {
        value: String,
    },
    Unstake {
        value: String,
    },
    Deploy {
        name: String,
        prover: ProgramMetadata,
        verifier: ProgramMetadata,
    },
    Run {
        workflow: Workflow,
    },
    Proof {
        parent: String,
        prover: String,
        proof: String,
        files: Vec<TxOutputFile>,
    },
    ProofKey {
        parent: String,
        key: String,
    },
    Verification {
        parent: String,
        verifier: String,
        verification: String,
        files: Vec<TxOutputFile>,
    },
    Cancel {
        parent: String,
    },
}

impl TxRpcPayload {
    pub fn from_tx_payload(payload: Payload, tx_hash: Hash, host: SocketAddr) -> Self {
        match payload {
            Payload::Empty => TxRpcPayload::Empty,
            Payload::Transfer { to, value } => TxRpcPayload::Transfer {
                to: hex::encode(to.serialize()),
                value: value.to_string(),
            },
            Payload::Stake { value } => TxRpcPayload::Stake {
                value: value.to_string(),
            },
            Payload::Unstake { value } => TxRpcPayload::Unstake {
                value: value.to_string(),
            },
            Payload::Deploy {
                name,
                prover,
                verifier,
            } => TxRpcPayload::Deploy {
                name,
                prover,
                verifier,
            },
            Payload::Run { workflow } => TxRpcPayload::Run { workflow },
            Payload::Proof {
                parent,
                prover,
                proof,
                files,
            } => {
                let (proof, files) = convert_payload_ouput(proof, files, tx_hash, host);

                TxRpcPayload::Proof {
                    parent: parent.to_string(),
                    prover: prover.to_string(),
                    proof,
                    files,
                }
            }
            Payload::ProofKey { parent, key } => TxRpcPayload::ProofKey {
                parent: parent.to_string(),
                key: hex::encode(key),
            },
            Payload::Verification {
                parent,
                verifier,
                verification,
                files,
            } => {
                let (verification, files) =
                    convert_payload_ouput(verification, files, tx_hash, host);
                TxRpcPayload::Verification {
                    parent: parent.to_string(),
                    verifier: verifier.to_string(),
                    verification,
                    files,
                }
            }
            Payload::Cancel { parent } => TxRpcPayload::Cancel {
                parent: parent.to_string(),
            },
        }
    }
}

fn convert_payload_ouput(
    proof: Vec<u8>,
    files: Vec<TxFile<Output>>,
    tx_hash: Hash,
    host: SocketAddr,
) -> (String, Vec<TxOutputFile>) {
    let files = files
        .into_iter()
        .map(|f| TxOutputFile::from_txfile(f, tx_hash, crate::HTTP_SERVER_SCHEME, host))
        .collect();
    let proof = base64::engine::general_purpose::STANDARD.encode(proof);
    (proof, files)
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct RpcTransaction {
    pub author: String,
    pub hash: String,
    pub payload: TxRpcPayload,
    pub nonce: u64,
    pub signature: String,
}

impl RpcTransaction {
    pub fn from_tx_validated(tx: Transaction<Validated>, host: SocketAddr) -> Self {
        RpcTransaction {
            author: hex::encode(tx.author.serialize()),
            hash: tx.hash.to_string(),
            nonce: tx.nonce,
            signature: tx.signature.to_string(),
            payload: TxRpcPayload::from_tx_payload(tx.payload, tx.hash, host),
        }
    }
}
