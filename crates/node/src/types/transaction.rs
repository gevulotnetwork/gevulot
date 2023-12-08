use super::hash::Hash;
use super::signature::Signature;
use libsecp256k1::PublicKey;
use num_bigint::BigInt;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct ProgramMetadata {
    pub name: String,
    pub hash: Hash,
    pub image_file_name: String,
    pub image_file_url: String,
    pub image_file_checksum: String,
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

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct WorkflowStep {
    pub program: Hash,
    pub args: Vec<String>,
    pub inputs: Vec<ProgramData>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct Workflow {
    pub steps: Vec<WorkflowStep>,
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

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct Transaction {
    pub hash: Hash,
    pub payload: Payload,
    pub nonce: u64,
    pub signature: Signature,
    #[serde(skip_serializing, skip_deserializing)]
    pub(crate) propagated: bool,
}
