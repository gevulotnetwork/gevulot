use std::sync::Arc;

use eyre::Result;
use gevulot_node::types::{
    transaction::{Payload, ProgramData, Workflow, WorkflowStep},
    File, Hash, Task, Transaction,
};
use thiserror::Error;
use uuid::Uuid;

#[allow(clippy::enum_variant_names)]
#[derive(Error, Debug, PartialEq)]
pub enum WorkflowError {
    #[error("incompatible transaction: {0}")]
    IncompatibleTransaction(String),

    #[error("workflow transaction missing: {0}")]
    WorkflowTransactionMissing(String),

    #[error("workflow step missing: {0}")]
    WorkflowStepMissing(String),
}

pub trait TransactionStore {
    fn find_transaction(&self, tx_hash: &Hash) -> Result<Option<Transaction>>;
}

pub struct WorkflowEngine {
    tx_store: Arc<dyn TransactionStore>,
}

impl WorkflowEngine {
    pub fn new(tx_store: Arc<dyn TransactionStore>) -> Self {
        WorkflowEngine { tx_store }
    }

    pub fn next_task(&self, cur_tx: &Transaction, workflow: &Workflow) -> Result<Option<Task>> {
        match &cur_tx.payload {
            Payload::Run { workflow } => {
                if workflow.steps.len() == 0 {
                    Ok(None)
                } else {
                    Ok(Some(self.workflow_step_to_task(&workflow.steps[0])))
                }
            }
            Payload::ProofKey { parent, key } => {
                let proof_tx = match self.tx_store.find_transaction(parent) {
                    Ok(None) => {
                        return Err(WorkflowError::WorkflowTransactionMissing(format!(
                            "Proof tx, hash {}",
                            parent.to_string()
                        ))
                        .into());
                    }
                    Ok(Some(tx)) => tx,
                    Err(err) => return Err(err.into()),
                };

                // TODO: Rewrite this spaghetti!

                if let Payload::Proof {
                    parent,
                    prover,
                    proof,
                } = proof_tx.payload
                {
                    match workflow.steps.iter().position(|s| s.program == prover) {
                        Some(proof_step_idx) => {
                            if workflow.steps.len() <= proof_step_idx {
                                Err(WorkflowError::WorkflowStepMissing(format!(
                                    "verifier for proof tx {}",
                                    proof_tx.hash
                                ))
                                .into())
                            } else {
                                Ok(Some(self.workflow_step_to_task(
                                    &workflow.steps[proof_step_idx + 1],
                                )))
                            }
                        }
                        None => Err(WorkflowError::WorkflowStepMissing(format!(
                            "verifier for proof tx {}",
                            proof_tx.hash
                        ))
                        .into()),
                    }
                } else {
                    Err(WorkflowError::IncompatibleTransaction(proof_tx.hash.to_string()).into())
                }
            }
            _ => Err(WorkflowError::IncompatibleTransaction(
                "unsupported payload type".to_string(),
            )
            .into()),
        }
    }

    fn workflow_step_to_task(&self, step: &WorkflowStep) -> Task {
        let id = Uuid::new_v4();
        let files = step
            .inputs
            .iter()
            .map(|e| match e {
                ProgramData::Input {
                    file_name,
                    file_url,
                    ..
                } => File {
                    task_id: id,
                    name: file_name.clone(),
                    url: file_url.clone(),
                },
                ProgramData::Output {
                    source_program,
                    file_name,
                } => File {
                    task_id: id,
                    name: file_name.clone(),
                    url: "".to_string(),
                },
            })
            .collect();

        Task {
            id,
            name: format!("{}-{}", id.to_string(), step.program.to_string()),
            kind: gevulot_node::types::TaskKind::Proof,
            program_id: step.program,
            args: step.args.clone(),
            files,
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use gevulot_node::types::{
        transaction::{Payload, ProgramData, Workflow, WorkflowStep},
        Hash, Signature, TaskKind, Transaction,
    };
    use libsecp256k1::SecretKey;
    use rand::{rngs::StdRng, SeedableRng};

    use super::*;

    pub struct TxStore {
        pub txs: HashMap<Hash, Transaction>,
    }

    impl TxStore {
        pub fn new(txs: &[Transaction]) -> Self {
            let mut store = TxStore {
                txs: HashMap::with_capacity(txs.len()),
            };
            txs.iter().for_each(|tx| {
                store.txs.insert(tx.hash, tx.clone());
            });
            store
        }
    }

    impl TransactionStore for TxStore {
        fn find_transaction(&self, tx_hash: &Hash) -> Result<Option<Transaction>> {
            Ok(self.txs.get(tx_hash).map(|e| e.clone()))
        }
    }

    #[test]
    fn test_next_task_for_empty_workflow_steps() {
        let wfe = WorkflowEngine::new(Arc::new(TxStore::new(&[])));
        let tx = transaction_for_workflow_steps(vec![]);
        if let Payload::Run { workflow } = &tx.payload {
            let task = wfe.next_task(&tx, &workflow).expect("next_task");
            assert!(task.is_none());
        }
    }

    #[test]
    fn test_next_task_for_simple_workflow_steps() {
        let wfe = WorkflowEngine::new(Arc::new(TxStore::new(&[])));
        let rng = &mut StdRng::from_entropy();
        let prover_hash = Hash::random(rng);

        let proving = WorkflowStep {
            program: prover_hash,
            args: vec![],
            inputs: vec![],
        };

        let verifying = WorkflowStep {
            program: Hash::random(rng),
            args: vec![],
            inputs: vec![ProgramData::Output {
                source_program: prover_hash,
                file_name: "proof.dat".to_string(),
            }],
        };

        let tx = transaction_for_workflow_steps(vec![proving.clone(), verifying]);
        if let Payload::Run { workflow } = &tx.payload {
            let task = wfe.next_task(&tx, &workflow).expect("next_task").unwrap();
            assert_eq!(task.kind, TaskKind::Proof);
            assert_eq!(task.program_id, proving.program);
            assert_eq!(task.args, Vec::<String>::new());
        };
    }

    #[test]
    fn test_next_task_for_verification() {
        let rng = &mut StdRng::from_entropy();
        let prover_hash = Hash::random(rng);
        let verifier_hash = Hash::random(rng);

        let proving = WorkflowStep {
            program: prover_hash,
            args: vec![],
            inputs: vec![],
        };

        let verifying = WorkflowStep {
            program: verifier_hash,
            args: vec![],
            inputs: vec![ProgramData::Output {
                source_program: prover_hash,
                file_name: "proof.dat".to_string(),
            }],
        };

        let workflow_steps = vec![proving.clone(), verifying];
        let workflow = Workflow {
            steps: workflow_steps.clone(),
        };

        let root_tx = transaction_for_workflow_steps(workflow_steps);
        let proof_tx = transaction_for_proof(&root_tx.hash, &prover_hash);
        let proofkey_tx = transaction_for_proofkey(&proof_tx.hash);
        let verification_tx = transaction_for_verification(&proof_tx.hash, &verifier_hash);
        let tx_store = TxStore::new(&[root_tx, proof_tx, proofkey_tx.clone(), verification_tx]);
        let wfe = WorkflowEngine::new(Arc::new(tx_store));

        let task = wfe.next_task(&proofkey_tx, &workflow);
        assert!(task.is_ok());
    }

    fn transaction_for_workflow_steps(steps: Vec<WorkflowStep>) -> Transaction {
        let key = SecretKey::random(&mut StdRng::from_entropy());
        let mut tx = Transaction {
            hash: Hash::default(),
            payload: Payload::Run {
                workflow: Workflow { steps },
            },
            nonce: 1,
            signature: Signature::default(),
            propagated: false,
        };

        tx.sign(&key);
        tx
    }

    fn transaction_for_proof(parent: &Hash, program: &Hash) -> Transaction {
        let key = SecretKey::random(&mut StdRng::from_entropy());
        let mut tx = Transaction {
            hash: Hash::default(),
            payload: Payload::Proof {
                parent: parent.clone(),
                prover: program.clone(),
                proof: "proof.".into(),
            },
            nonce: 1,
            signature: Signature::default(),
            propagated: false,
        };

        tx.sign(&key);
        tx
    }

    fn transaction_for_proofkey(parent: &Hash) -> Transaction {
        let key = SecretKey::random(&mut StdRng::from_entropy());
        let mut tx = Transaction {
            hash: Hash::default(),
            payload: Payload::ProofKey {
                parent: parent.clone(),
                key: "key.".into(),
            },
            nonce: 1,
            signature: Signature::default(),
            propagated: false,
        };

        tx.sign(&key);
        tx
    }

    fn transaction_for_verification(parent: &Hash, program: &Hash) -> Transaction {
        let key = SecretKey::random(&mut StdRng::from_entropy());
        let mut tx = Transaction {
            hash: Hash::default(),
            payload: Payload::Verification {
                parent: parent.clone(),
                verifier: program.clone(),
                verification: String::from("verification."),
            },
            nonce: 1,
            signature: Signature::default(),
            propagated: false,
        };

        tx.sign(&key);
        tx
    }
}
