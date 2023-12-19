use std::sync::Arc;

use super::hash::Hash;
use super::signature::Signature;
use libsecp256k1::{sign, verify, Message, PublicKey, SecretKey};
use num_bigint::BigInt;
use serde::{Deserialize, Serialize};
use sha3::{Digest, Sha3_256};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub enum TransactionTree {
    Root {
        children: Vec<Arc<TransactionTree>>,
        hash: Hash,
    },
    Node {
        parent: Arc<TransactionTree>,
        children: Vec<Arc<TransactionTree>>,
        hash: Hash,
    },
    Leaf {
        parent: Arc<TransactionTree>,
        hash: Hash,
    },
}

impl Default for TransactionTree {
    fn default() -> Self {
        TransactionTree::Root {
            children: vec![],
            hash: Hash::default(),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct ProgramMetadata {
    pub name: String,
    /// Program hash. Used to identify `Program`.
    pub hash: Hash,
    pub image_file_name: String,
    pub image_file_url: String,
    // Image file checksum is BLAKE3 hash of the file.
    pub image_file_checksum: String,
}

impl ProgramMetadata {
    pub fn update_hash(&mut self) {
        let mut hasher = Sha3_256::new();
        hasher.update(self.name.as_bytes());
        hasher.update(self.image_file_name.as_bytes());
        hasher.update(self.image_file_url.as_bytes());
        hasher.update(self.image_file_checksum.as_bytes());
        self.hash = hasher.finalize()[0..32].into();
    }

    pub fn serialize_into(&self, buf: &mut Vec<u8>) {
        buf.append(&mut self.name.as_bytes().to_vec());
        buf.append(&mut self.hash.to_vec());
        buf.append(&mut self.image_file_name.as_bytes().to_vec());
        buf.append(&mut self.image_file_url.as_bytes().to_vec());
        buf.append(&mut self.image_file_checksum.as_bytes().to_vec());
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub enum ProgramData {
    Input {
        file_name: String,
        file_url: String,
        checksum: String,
    },
    Output {
        source_program: Hash,
        file_name: String,
    },
}

impl ProgramData {
    fn serialize_into(&self, buf: &mut Vec<u8>) {
        match self {
            ProgramData::Input {
                file_name,
                file_url,
                checksum,
            } => {
                buf.append(&mut file_name.as_bytes().to_vec());
                buf.append(&mut file_url.as_bytes().to_vec());
                buf.append(&mut checksum.as_bytes().to_vec());
            }
            ProgramData::Output {
                source_program,
                file_name,
            } => {
                buf.append(&mut source_program.to_vec());
                buf.append(&mut file_name.as_bytes().to_vec());
            }
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct WorkflowStep {
    pub program: Hash,
    pub args: Vec<String>,
    pub inputs: Vec<ProgramData>,
}

impl WorkflowStep {
    fn serialize_into(&self, buf: &mut Vec<u8>) {
        buf.append(&mut self.program.to_vec());
        self.args
            .iter()
            .for_each(|e| buf.append(&mut e.as_bytes().to_vec()));
        self.inputs.iter().for_each(|e| e.serialize_into(buf));
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct Workflow {
    pub steps: Vec<WorkflowStep>,
}

impl Workflow {
    fn serialize_into(&self, buf: &mut Vec<u8>) {
        self.steps.iter().for_each(|e| e.serialize_into(buf));
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub enum Payload {
    #[default]
    Empty,
    Transfer {
        to: PublicKey,
        value: BigInt,
    },
    Stake {
        value: BigInt,
    },
    Unstake {
        value: BigInt,
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
        parent: Hash,
        proof: String,
    },
    ProofKey {
        parent: Hash,
        key: String,
    },
    Verification {
        parent: Hash,
        verification: String,
    },
    Cancel {
        parent: Hash,
    },
}

impl Payload {
    fn serialize_into(&self, buf: &mut Vec<u8>) {
        match self {
            Payload::Empty => {}
            Payload::Transfer { to, value } => {
                buf.append(&mut to.serialize().to_vec());
                buf.append(&mut value.to_signed_bytes_be());
            }
            Payload::Stake { value } => {
                buf.append(&mut value.to_signed_bytes_be());
            }
            Payload::Unstake { value } => {
                buf.append(&mut value.to_signed_bytes_be());
            }
            Payload::Deploy {
                name,
                prover,
                verifier,
            } => {
                buf.append(&mut name.as_bytes().to_vec());
                prover.serialize_into(buf);
                verifier.serialize_into(buf);
            }
            Payload::Run { workflow } => {
                workflow.serialize_into(buf);
            }
            Payload::Proof { parent, proof } => {
                buf.append(&mut parent.to_vec());
                buf.append(&mut proof.as_bytes().to_vec());
            }
            Payload::ProofKey { parent, key } => {
                buf.append(&mut parent.to_vec());
                buf.append(&mut key.as_bytes().to_vec());
            }
            Payload::Verification {
                parent,
                verification,
            } => {
                buf.append(&mut parent.to_vec());
                buf.append(&mut verification.as_bytes().to_vec());
            }
            Payload::Cancel { parent } => {
                buf.append(&mut parent.to_vec());
            }
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct Transaction {
    pub hash: Hash,
    pub payload: Payload,
    pub nonce: u64,
    pub signature: Signature,
    #[serde(skip_serializing, skip_deserializing)]
    pub propagated: bool,
}

impl Transaction {
    pub fn sign(&mut self, key: &SecretKey) {
        // Refresh transaction hash before signing.
        self.hash = self.compute_hash();
        let msg: Message = self.hash.into();
        let (sig, _) = sign(&msg, key);
        self.signature = sig.into();
    }

    pub fn verify(&self, pub_key: &PublicKey) -> bool {
        let hash = self.compute_hash();
        let msg: Message = hash.into();
        verify(&msg, &self.signature.into(), pub_key)
    }

    pub fn compute_hash(&self) -> Hash {
        let mut hasher = Sha3_256::new();
        let mut buf = vec![];
        self.payload.serialize_into(&mut buf);
        hasher.update(buf);
        hasher.update(self.nonce.to_be_bytes());
        (&hasher.finalize()[0..32]).into()
    }
}
