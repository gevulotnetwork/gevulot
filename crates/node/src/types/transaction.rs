use super::hash::Hash;
use super::signature::Signature;
use libsecp256k1::PublicKey;
use num_bigint::BigInt;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ProgramMetadata {
    name: String,
    image_file_name: String,
    image_url: String,
    image_checksum: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
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

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct WorkflowStep {
    program: Hash,
    args: Vec<String>,
    inputs: Vec<ProgramData>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Workflow {
    steps: Vec<WorkflowStep>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
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

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Transaction {
    pub hash: Hash,
    pub payload: Payload,
    pub nonce: u64,
    pub signature: Signature,
    #[serde(skip_serializing, skip_deserializing)]
    pub(crate) propagated: bool,
}
