use crate::types::Hash;
use libsecp256k1::{PublicKey, SecretKey};
use num_bigint::BigInt;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize, sqlx::FromRow)]
pub struct Transfer {
    pub to: PublicKey,
    pub value: BigInt,
}

impl Default for Transfer {
    fn default() -> Self {
        Transfer {
            to: PublicKey::from_secret_key(&SecretKey::default()),
            value: BigInt::default(),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, sqlx::FromRow)]
pub struct Stake {
    pub value: BigInt,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, sqlx::FromRow)]
pub struct Unstake {
    pub value: BigInt,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, sqlx::FromRow)]
pub struct Deploy {
    pub name: String,
    /// prover field is the hash of the corresponding prover program.
    pub prover: Hash,
    /// verifier field is the hash of the corresponding verifyier program.
    pub verifier: Hash,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, sqlx::FromRow)]
pub struct WorkflowStep {
    pub id: Option<i64>,
    pub tx: Hash,
    pub sequence: i64,
    pub program: Hash,
    pub args: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, sqlx::FromRow)]
pub struct ProgramInputData {
    pub workflow_step_id: i64,
    pub file_name: String,
    pub file_url: String,
    pub checksum: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, sqlx::FromRow)]
pub struct ProgramOutputData {
    pub workflow_step_id: i64,
    pub source_program: Hash,
    pub file_name: String,
}
