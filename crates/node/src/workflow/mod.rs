use eyre::Result;
use gevulot_node::types::{
    transaction::{Payload, ProgramData, Workflow, WorkflowStep},
    File, Task, Transaction,
};
use thiserror::Error;
use uuid::Uuid;

#[allow(clippy::enum_variant_names)]
#[derive(Error, Debug, PartialEq)]
pub enum WorkflowError {
    #[error("incompatible transaction: {0}")]
    IncompatibleTransaction(String),
}

pub struct WorkflowEngine {}

impl WorkflowEngine {
    pub fn new() -> Self {
        WorkflowEngine {}
    }

    //////////////////////////////////////////////////////////////////////////////////////////////////////
    //////////////////////////////////////////////////////////////////////////////////////////////////////
    //////////////////////////////////////////////////////////////////////////////////////////////////////
    //////////////////////////////////////////////////////////////////////////////////////////////////////
    //////////////////////////   REFACTOR THIS INTO BLOCKCHAIN EVENT CONTROLLER    ///////////////////////
    //////////////////////////////////////////////////////////////////////////////////////////////////////
    //////////////////////////////////////////////////////////////////////////////////////////////////////
    //////////////////////////////////////////////////////////////////////////////////////////////////////
    //////////////////////////////////////////////////////////////////////////////////////////////////////
    //////////////////////////////////////////////////////////////////////////////////////////////////////
    //////////////////////////////////////////////////////////////////////////////////////////////////////

    pub fn next_task(
        &self,
        cur_tx: &Transaction,
        parent_tx: Option<&Transaction>,
        workflow: &Workflow,
    ) -> Result<Option<Task>> {
        match &cur_tx.payload {
            Payload::Run { workflow } => {
                if workflow.steps.len() == 0 {
                    Ok(None)
                } else {
                    Ok(Some(self.workflow_step_to_task(&workflow.steps[0])))
                }
            }
            Payload::ProofKey { parent, key } => Ok(None),
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

        let t = Task {
            id,
            name: format!("{}-{}", id.to_string(), step.program.to_string()),
            kind: gevulot_node::types::TaskKind::Proof,
            program_id: step.program,
            args: step.args.clone(),
            files,
            ..Default::default()
        };
        Task::default()
    }
}

#[cfg(test)]
mod tests {
    use gevulot_node::types::{
        transaction::{Payload, ProgramData, Workflow, WorkflowStep},
        Hash, Signature, Transaction,
    };
    use libsecp256k1::SecretKey;
    use rand::{rngs::StdRng, SeedableRng};

    use super::*;

    #[test]
    fn test_next_task_for_empty_workflow_steps() {
        let wfe = WorkflowEngine::new();
        let tx = transaction_for_workflow_steps(vec![]);
        if let Payload::Run { workflow } = &tx.payload {
            let task = wfe.next_task(&tx, None, &workflow).expect("next_task");
            assert!(task.is_none());
        }
    }

    #[test]
    fn test_next_task_for_simple_workflow_steps() {
        let wfe = WorkflowEngine::new();
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

        let tx = transaction_for_workflow_steps(vec![proving, verifying]);
        if let Payload::Run { workflow } = &tx.payload {
            let task = wfe.next_task(&tx, None, &workflow).expect("next_task");
            assert!(task.is_some());
        };
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
}
