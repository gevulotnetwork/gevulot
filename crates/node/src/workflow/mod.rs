use std::sync::Arc;

use async_trait::async_trait;
use eyre::Result;
use gevulot_node::types::{
    transaction::{Payload, ProgramData, Workflow, WorkflowStep},
    File, Hash, Task, TaskKind, Transaction,
};
use thiserror::Error;
use uuid::Uuid;

use crate::storage;

#[allow(clippy::enum_variant_names)]
#[derive(Error, Debug, PartialEq)]
pub enum WorkflowError {
    #[error("incompatible transaction: {0}")]
    IncompatibleTransaction(String),

    #[error("workflow transaction missing: {0}")]
    WorkflowTransactionMissing(String),

    #[error("workflow step missing: {0}")]
    WorkflowStepMissing(String),

    #[error("transaction not found: {0}")]
    TransactionNotFound(Hash),
}

#[async_trait]
pub trait TransactionStore: Sync + Send {
    async fn find_transaction(&self, tx_hash: &Hash) -> Result<Option<Transaction>>;
}

pub struct WorkflowEngine {
    tx_store: Arc<dyn TransactionStore>,
    file_storage: Arc<storage::File>,
}

impl WorkflowEngine {
    pub fn new(tx_store: Arc<dyn TransactionStore>, file_storage: Arc<storage::File>) -> Self {
        WorkflowEngine {
            tx_store,
            file_storage,
        }
    }

    pub async fn next_task(&self, cur_tx: &Transaction) -> Result<Option<Task>> {
        let workflow = self.workflow_for_transaction(&cur_tx.hash).await?;

        match &cur_tx.payload {
            Payload::Run { workflow } => {
                tracing::debug!("creating next task from Run tx {}", &cur_tx.hash);

                if workflow.steps.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(
                        self.workflow_step_to_task(
                            cur_tx.hash,
                            &workflow.steps[0],
                            TaskKind::Proof,
                        )
                        .await?,
                    ))
                }
            }
            Payload::Proof {
                parent,
                prover,
                proof,
            } => {
                tracing::debug!("creating next task from Proof tx {}", &cur_tx.hash);

                match workflow.steps.iter().position(|s| s.program == *prover) {
                    Some(proof_step_idx) => {
                        if proof_step_idx <= workflow.steps.len() {
                            Err(WorkflowError::WorkflowStepMissing(format!(
                                "verifier for proof tx {}",
                                cur_tx.hash.clone(),
                            ))
                            .into())
                        } else {
                            Ok(Some(
                                self.workflow_step_to_task(
                                    cur_tx.hash,
                                    &workflow.steps[proof_step_idx + 1],
                                    TaskKind::Verification,
                                )
                                .await?,
                            ))
                        }
                    }
                    None => Err(WorkflowError::WorkflowStepMissing(format!(
                        "verifier for proof tx {}",
                        cur_tx.hash
                    ))
                    .into()),
                }
            }
            Payload::ProofKey { parent, key } => {
                tracing::debug!("creating next task from ProofKey tx {}", &cur_tx.hash);

                let proof_tx = match self.tx_store.find_transaction(parent).await {
                    Ok(None) => {
                        return Err(WorkflowError::WorkflowTransactionMissing(format!(
                            "Proof tx, hash {}",
                            parent
                        ))
                        .into());
                    }
                    Ok(Some(tx)) => tx,
                    Err(err) => return Err(err),
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
                                Ok(Some(
                                    self.workflow_step_to_task(
                                        proof_tx.hash,
                                        &workflow.steps[proof_step_idx + 1],
                                        TaskKind::Verification,
                                    )
                                    .await?,
                                ))
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

    async fn find_parent_tx_for_program(&self, tx_hash: &Hash, program: &Hash) -> Result<Hash> {
        let mut cur_tx = *tx_hash;

        tracing::debug!("finding workflow for transaction {}", tx_hash);

        // Traverse transaction tree up by tracing parent until the right tx is found.
        loop {
            let tx = self.tx_store.find_transaction(&cur_tx).await?;

            if tx.is_none() {
                return Err(WorkflowError::TransactionNotFound(cur_tx).into());
            }

            match tx.unwrap().payload {
                Payload::Run { workflow } => {
                    if workflow.steps.is_empty() {
                        return Err(WorkflowError::TransactionNotFound(cur_tx).into());
                    }

                    if workflow.steps.first().unwrap().program == *program {
                        return Ok(cur_tx);
                    } else {
                        return Err(WorkflowError::TransactionNotFound(cur_tx).into());
                    }
                }
                Payload::Proof { parent, prover, .. } => {
                    if &cur_tx != tx_hash && prover == *program {
                        return Ok(cur_tx);
                    }

                    cur_tx = parent;
                    continue;
                }
                Payload::ProofKey { parent, .. } => {
                    cur_tx = parent;
                    continue;
                }
                Payload::Verification {
                    parent, verifier, ..
                } => {
                    if &cur_tx != tx_hash && verifier == *program {
                        return Ok(cur_tx);
                    }

                    cur_tx = parent;
                    continue;
                }
                _ => {
                    tracing::debug!(
                        "failed to find workflow for transaction {}: incompatible transaction",
                        cur_tx
                    );
                    return Err(WorkflowError::IncompatibleTransaction(cur_tx.to_string()).into());
                }
            }
        }
    }

    async fn workflow_for_transaction(&self, tx_hash: &Hash) -> Result<Workflow> {
        let mut tx_hash = *tx_hash;

        tracing::debug!("finding workflow for transaction {}", tx_hash);

        // Traverse transaction tree up by tracing parent until
        // Payload::Run is found.
        loop {
            let tx = self.tx_store.find_transaction(&tx_hash).await?;

            if tx.is_none() {
                return Err(WorkflowError::TransactionNotFound(tx_hash).into());
            }

            match tx.unwrap().payload {
                Payload::Run { workflow } => {
                    tracing::debug!("workflow found for transaction {}", tx_hash);
                    return Ok(workflow);
                }
                Payload::Proof { parent, .. } => {
                    tracing::debug!("finding workflow from parent {} of {}", &parent, tx_hash);
                    tx_hash = parent;
                    continue;
                }
                Payload::ProofKey { parent, .. } => {
                    tracing::debug!("finding workflow from parent {} of {}", &parent, tx_hash);
                    tx_hash = parent;
                    continue;
                }
                Payload::Verification { parent, .. } => {
                    tracing::debug!("finding workflow from parent {} of {}", &parent, tx_hash);
                    tx_hash = parent;
                    continue;
                }
                _ => {
                    tracing::debug!(
                        "failed to find workflow for transaction {}: incompatible transaction",
                        &tx_hash
                    );
                    return Err(WorkflowError::IncompatibleTransaction(tx_hash.to_string()).into());
                }
            }
        }
    }

    async fn workflow_step_to_task(
        &self,
        tx: Hash,
        step: &WorkflowStep,
        kind: TaskKind,
    ) -> Result<Task> {
        let id = Uuid::new_v4();
        let mut file_transfers = vec![];
        let files = step
            .inputs
            .iter()
            .map(|e| match e {
                ProgramData::Input {
                    file_name,
                    file_url,
                    ..
                } => File {
                    tx,
                    name: file_name.clone(),
                    url: file_url.clone(),
                },
                ProgramData::Output {
                    source_program,
                    file_name,
                } => {
                    // Make record of file that needs transfer from source tx to current tx's files.
                    file_transfers.push((*source_program, file_name.clone()));

                    File {
                        tx,
                        name: file_name.clone(),
                        url: "".to_string(),
                    }
                }
            })
            .collect();

        // Process file transfers from source programs.
        for (source_program, file_name) in file_transfers {
            let source_tx = self
                .find_parent_tx_for_program(&tx, &source_program)
                .await
                .expect("output file dependency missing");

            self.file_storage
                .move_task_file(&source_tx.to_string(), &tx.to_string(), &file_name)
                .await?;
        }

        Ok(Task {
            id,
            tx,
            name: format!("{}-{}", id, step.program),
            kind,
            program_id: step.program,
            args: step.args.clone(),
            files,
            ..Default::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, env::temp_dir};

    use gevulot_node::types::{
        transaction::{Payload, ProgramData, Workflow, WorkflowStep},
        Hash, TaskKind, Transaction,
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

    #[async_trait]
    impl TransactionStore for TxStore {
        async fn find_transaction(&self, tx_hash: &Hash) -> Result<Option<Transaction>> {
            Ok(self.txs.get(tx_hash).cloned())
        }
    }

    #[tokio::test]
    async fn test_next_task_for_empty_workflow_steps() {
        let wfe = WorkflowEngine::new(
            Arc::new(TxStore::new(&[])),
            Arc::new(storage::File::new(&temp_dir())),
        );
        let tx = transaction_for_workflow_steps(vec![]);
        if let Payload::Run { workflow } = &tx.payload {
            let res = wfe.next_task(&tx).await;
            assert!(res.is_err());
        }
    }

    #[tokio::test]
    async fn test_next_task_for_simple_workflow_steps() {
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
        let wfe = WorkflowEngine::new(
            Arc::new(TxStore::new(&[tx.clone()])),
            Arc::new(storage::File::new(&temp_dir())),
        );

        if let Payload::Run { workflow } = &tx.payload {
            let task = wfe.next_task(&tx).await.expect("next_task").unwrap();
            assert_eq!(task.kind, TaskKind::Proof);
            assert_eq!(task.program_id, proving.program);
            assert_eq!(task.args, Vec::<String>::new());
        };
    }

    #[tokio::test]
    async fn test_next_task_for_verification() {
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
            inputs: vec![],
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
        let wfe = WorkflowEngine::new(
            Arc::new(tx_store),
            Arc::new(storage::File::new(&temp_dir())),
        );

        let task = wfe.next_task(&proofkey_tx).await;
        assert!(task.is_ok());
    }

    fn transaction_for_workflow_steps(steps: Vec<WorkflowStep>) -> Transaction {
        let key = SecretKey::random(&mut StdRng::from_entropy());
        Transaction::new(
            Payload::Run {
                workflow: Workflow { steps },
            },
            &key,
        )
    }

    fn transaction_for_proof(parent: &Hash, program: &Hash) -> Transaction {
        let key = SecretKey::random(&mut StdRng::from_entropy());
        Transaction::new(
            Payload::Proof {
                parent: *parent,
                prover: *program,
                proof: "proof.".into(),
            },
            &key,
        )
    }

    fn transaction_for_proofkey(parent: &Hash) -> Transaction {
        let key = SecretKey::random(&mut StdRng::from_entropy());
        Transaction::new(
            Payload::ProofKey {
                parent: *parent,
                key: "key.".into(),
            },
            &key,
        )
    }

    fn transaction_for_verification(parent: &Hash, program: &Hash) -> Transaction {
        let key = SecretKey::random(&mut StdRng::from_entropy());
        Transaction::new(
            Payload::Verification {
                parent: *parent,
                verifier: *program,
                verification: b"verification.".to_vec(),
            },
            &key,
        )
    }
}
