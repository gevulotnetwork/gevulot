use super::file::{AssetFile, Image, Output, TxFile};
use super::signature::Signature;
use super::{hash::Hash, program::ResourceRequest};
use crate::types::transaction;
use eyre::Result;
use libsecp256k1::{sign, verify, Message, PublicKey, SecretKey};
use num_bigint::BigInt;
use serde::{Deserialize, Serialize};
use sha3::{Digest, Sha3_256};
use std::{collections::HashSet, rc::Rc};
use thiserror::Error;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub enum TransactionTree {
    Root {
        children: Vec<Rc<TransactionTree>>,
        hash: Hash,
    },
    Node {
        children: Vec<Rc<TransactionTree>>,
        hash: Hash,
    },
    Leaf {
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
    /// Image file checksum is BLAKE3 hash of the file.
    pub image_file_checksum: String,
    /// Program resource requirements for execution.
    pub resource_requirements: Option<ResourceRequest>,
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

#[allow(clippy::large_enum_variant)]
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
        prover: Hash,
        proof: Vec<u8>,
        files: Vec<TxFile<Output>>,
    },
    ProofKey {
        parent: Hash,
        key: Vec<u8>,
    },
    Verification {
        parent: Hash,
        verifier: Hash,
        verification: Vec<u8>,
        files: Vec<TxFile<Output>>,
    },
    Cancel {
        parent: Hash,
    },
}

impl std::fmt::Display for Payload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let payload = match self {
            Payload::Empty => "Empty",
            Payload::Transfer { .. } => "Transfer",
            Payload::Stake { .. } => "Stake",
            Payload::Unstake { .. } => "Unstake",
            Payload::Deploy { .. } => "Deploy",
            Payload::Run { .. } => "Run",
            Payload::Proof { .. } => "Proof",
            Payload::ProofKey { .. } => "ProofKey",
            Payload::Verification { .. } => "Verification",
            Payload::Cancel { .. } => "Cancel",
        };
        write!(f, "({})", payload)
    }
}

impl Payload {
    // Return the parent tx associated to the payloads, if any.
    pub fn get_parent_tx(&self) -> Option<&Hash> {
        match self {
            Payload::Proof { parent, .. } => Some(parent),
            Payload::ProofKey { parent, .. } => Some(parent),
            Payload::Verification { parent, .. } => Some(parent),
            _ => None,
        }
    }
    pub fn serialize_into(&self, buf: &mut Vec<u8>) {
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
            Payload::Proof {
                parent,
                prover,
                proof,
                files,
            } => {
                buf.append(&mut parent.to_vec());
                buf.append(&mut prover.to_vec());
                buf.append(proof.clone().as_mut());
                buf.append(proof.clone().as_mut());
                buf.append(&mut TxFile::<Output>::vec_to_bytes(files).unwrap());
            }
            Payload::ProofKey { parent, key } => {
                buf.append(&mut parent.to_vec());
                buf.append(key.clone().as_mut());
            }
            Payload::Verification {
                parent,
                verifier,
                verification,
                files,
            } => {
                buf.append(&mut parent.to_vec());
                buf.append(&mut verifier.to_vec());
                buf.append(verification.clone().as_mut());
                buf.append(&mut TxFile::<Output>::vec_to_bytes(files).unwrap());
            }
            Payload::Cancel { parent } => {
                buf.append(&mut parent.to_vec());
            }
        }
    }
}

#[allow(clippy::enum_variant_names)]
#[derive(Error, Debug)]
pub enum TransactionError {
    #[error("validation: {0}")]
    Validation(String),
    #[error("General error: {0}")]
    General(String),
}

// Transaction definition.
// Type state are use to define the different state of a Tx.
//
// Tx are defined in 3 domains: Validation, Execution, Storage.
// Currently the same definition is used but different type should be defined  (TODO).
// Only the validation type state are defined.
// Created : identify a Tx that has just been created.
// Received: Identify a Tx that has been received. Determine the received source.
// Validated: Identify a Tx that has been validated. Pass all the validation process.
// The validation suppose the Tx has been propagated. Currently there's no notification during the propagation (TODO).

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Created;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub enum Received {
    P2P,
    RPC,
    TXRESULT,
}

impl Received {
    fn is_from_tx_exec_result(&self) -> bool {
        matches!(self, Received::TXRESULT)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Validated;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Transaction<T> {
    pub author: PublicKey,
    pub hash: Hash,
    pub payload: Payload,
    pub nonce: u64,
    pub signature: Signature,
    #[serde(skip_serializing, skip_deserializing)]
    pub propagated: bool,
    #[serde(skip_serializing, skip_deserializing)]
    pub executed: bool,
    pub state: T,
}

impl<T> Transaction<T> {
    pub fn compute_hash(&self) -> Hash {
        let mut hasher = Sha3_256::new();
        let mut buf = vec![];
        hasher.update(self.author.serialize());
        self.payload.serialize_into(&mut buf);
        hasher.update(buf);
        hasher.update(self.nonce.to_be_bytes());
        (&hasher.finalize()[0..32]).into()
    }
}

impl Default for Transaction<Created> {
    fn default() -> Self {
        Self {
            author: PublicKey::from_secret_key(&SecretKey::default()),
            hash: Hash::default(),
            payload: Payload::default(),
            nonce: 0,
            signature: Signature::default(),
            propagated: false,
            executed: false,
            state: Created,
        }
    }
}

impl Default for Transaction<Validated> {
    fn default() -> Self {
        Self {
            author: PublicKey::from_secret_key(&SecretKey::default()),
            hash: Hash::default(),
            payload: Payload::default(),
            nonce: 0,
            signature: Signature::default(),
            propagated: false,
            executed: false,
            state: Validated,
        }
    }
}

impl Transaction<Created> {
    pub fn new(payload: Payload, signing_key: &SecretKey) -> Self {
        let author = PublicKey::from_secret_key(signing_key);

        let mut tx = Self {
            author,
            hash: Hash::default(),
            payload,
            nonce: 0,
            signature: Signature::default(),
            propagated: false,
            executed: false,
            state: Created,
        };

        tx.sign(signing_key);

        tracing::debug!(
            "Transaction::new tx:{} payload:{}",
            tx.hash.to_string(),
            tx.payload
        );

        tx
    }

    pub fn sign(&mut self, key: &SecretKey) {
        // Refresh transaction hash before signing.
        self.hash = self.compute_hash();
        let msg: Message = self.hash.into();
        let (sig, _) = sign(&msg, key);
        self.signature = sig.into();
    }

    pub fn into_received(self, state: Received) -> Transaction<Received> {
        Transaction {
            author: self.author,
            hash: self.hash,
            payload: self.payload,
            nonce: self.nonce,
            signature: self.signature,
            propagated: self.propagated,
            executed: self.executed,
            state,
        }
    }
}

impl Transaction<Received> {
    pub fn verify(&self) -> bool {
        let hash = self.compute_hash();
        let msg: Message = hash.into();
        verify(&msg, &self.signature.into(), &self.author)
    }

    pub fn validate(&self) -> Result<(), TransactionError> {
        if let Payload::Run { ref workflow } = self.payload {
            let mut programs = HashSet::new();
            for step in &workflow.steps {
                if !programs.insert(step.program) {
                    return Err(TransactionError::Validation(format!(
                        "multiple programs in workflow: {}",
                        &step.program
                    )));
                }
            }
        }

        if !self.verify() {
            return Err(TransactionError::Validation(String::from(
                "signature verification failed",
            )));
        }

        Ok(())
    }

    pub fn get_asset_list(&self) -> Result<Vec<AssetFile>> {
        tracing::trace!("get_asset_list Transaction<Received:{self:?}");
        match &self.payload {
            transaction::Payload::Deploy {
                prover, verifier, ..
            } => Ok(vec![
                TxFile::<Image>::try_from_prg_meta_data(prover).into(),
                TxFile::<Image>::try_from_prg_meta_data(verifier).into(),
            ]),
            Payload::Run { workflow } => {
                workflow
                    .steps
                    .iter()
                    .flat_map(|step| &step.inputs)
                    .filter_map(|input| {
                        match input {
                            ProgramData::Input {
                                file_name,
                                file_url,
                                checksum,
                            } => Some((file_name, file_url, checksum)),
                            ProgramData::Output { .. } => {
                                /* ProgramData::Output as input means it comes from another
                                program execution -> skip this branch. */
                                None
                            }
                        }
                    })
                    .map(|(file_name, file_url, checksum)| {
                        //verify the url is valide.
                        reqwest::Url::parse(file_url)?;
                        Ok(AssetFile::new(
                            file_name.to_string(),
                            file_url.clone(),
                            checksum.to_string().into(),
                            self.hash.to_string(),
                            false,
                        ))
                    })
                    .collect()
            }
            Payload::Proof { files, .. } | Payload::Verification { files, .. } => {
                //generated file during execution has already been moved. No Download.
                if self.state.is_from_tx_exec_result() {
                    Ok(vec![])
                } else {
                    files
                        .iter()
                        .map(|file| Ok(file.clone().into_download_file(self.hash)))
                        .collect()
                }
            }
            // Other transaction types don't have external assets that would
            // need processing.
            _ => Ok(vec![]),
        }
    }
}

#[cfg(test)]
mod tests {

    use rand::{rngs::StdRng, SeedableRng};

    use super::*;

    #[test]
    fn test_sign_and_verify_tx() {
        let sk = SecretKey::random(&mut StdRng::from_entropy());

        let tx = Transaction::new(Payload::Empty, &sk);
        let tx = tx.into_received(Received::P2P);
        assert!(tx.verify());
    }

    #[test]
    fn test_verify_fails_on_tamper() {
        let sk = SecretKey::random(&mut StdRng::from_entropy());

        let mut tx = Transaction::new(Payload::Empty, &sk);

        // Change nonce after signing.
        tx.nonce += 1;

        // Verify must return false.
        let tx = tx.into_received(Received::TXRESULT);
        assert!(!tx.verify());
    }

    #[test]
    fn test_tx_validate_ensures_unique_programs() {
        let prover = WorkflowStep::default();
        let verifier = WorkflowStep::default();

        let workflow = Workflow {
            // Both steps are `Default::default()` -> same program hash -> invalid.
            steps: vec![prover, verifier],
        };

        let sk = SecretKey::random(&mut StdRng::from_entropy());
        let tx = Transaction::<Created> {
            author: PublicKey::from_secret_key(&sk),
            payload: Payload::Run { workflow },
            signature: Signature::default(),
            ..Default::default()
        };

        let tx = tx.into_received(Received::RPC);
        assert!(tx.validate().is_err());
    }

    #[test]
    fn test_tx_validations_verifies_signature() {
        let tx = Transaction::<Created>::default();

        let tx = tx.into_received(Received::RPC);
        assert!(tx.validate().is_err());
    }
}
