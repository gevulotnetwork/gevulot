use eyre::Result;
use sqlx::{self, Row};
use uuid::Uuid;

use super::entity::{self};
use crate::types::{self, transaction::ProgramData, File, Hash, Program, Task};

#[derive(Clone)]
pub struct Database {
    pool: sqlx::PgPool,
}

// TODO: Split this into domain specific components.
impl Database {
    pub async fn new(db_url: &str) -> Result<Database> {
        let pool = sqlx::PgPool::connect(db_url).await?;
        Ok(Database { pool })
    }

    pub async fn add_program(&self, db_conn: &mut sqlx::PgConnection, p: &Program) -> Result<()> {
        sqlx::query!(
            "INSERT INTO program ( hash, name, image_file_name, image_file_url, image_file_checksum ) VALUES ( $1, $2, $3, $4, $5 ) RETURNING *",
            p.hash.to_string(),
            p.name,
            p.image_file_name,
            p.image_file_url,
            p.image_file_checksum,
        )
        .fetch_one(db_conn)
        .await?;
        Ok(())
    }

    pub async fn find_program(&self, hash: impl AsRef<Hash>) -> Result<Option<Program>> {
        // non-macro query_as used because of sqlx limitations with enums.
        let program = sqlx::query_as::<_, Program>("SELECT * FROM program WHERE hash = $1")
            .bind(hash.as_ref())
            .fetch_optional(&self.pool)
            .await?;

        Ok(program)
    }

    pub async fn get_program(
        &self,
        db_conn: &mut sqlx::PgConnection,
        hash: impl AsRef<Hash>,
    ) -> Result<Program> {
        // non-macro query_as used because of sqlx limitations with enums.
        let program = sqlx::query_as::<_, Program>("SELECT * FROM program WHERE hash = $1")
            .bind(hash.as_ref())
            .fetch_one(db_conn)
            .await?;

        Ok(program)
    }

    pub async fn get_programs(&self) -> Result<Vec<Program>> {
        let programs = sqlx::query_as::<_, Program>("SELECT * FROM program")
            .fetch_all(&self.pool)
            .await?;
        Ok(programs)
    }

    pub async fn add_task(&self, t: &Task) -> Result<()> {
        let mut tx = self.pool.begin().await?;

        if let Err(err) = sqlx::query(
            "INSERT INTO task ( id, name, args, state, program_id ) VALUES ( $1, $2, $3, $4, $5 )",
        )
        .bind(t.id)
        .bind(&t.name)
        .bind(&t.args)
        .bind(&t.state)
        .bind(t.program_id)
        .execute(&self.pool)
        .await
        {
            tx.rollback().await?;
            return Err(err.into());
        }

        {
            let mut query_builder =
                sqlx::QueryBuilder::new("INSERT INTO file ( task_id, name, url )");
            query_builder.push_values(&t.files, |mut b, new_file| {
                b.push_bind(t.id)
                    .push_bind(&new_file.name)
                    .push_bind(&new_file.url);
            });

            let query = query_builder.build();
            if let Err(err) = query.execute(&mut *tx).await {
                tx.rollback().await?;
                return Err(err.into());
            }
        }

        tx.commit().await.map_err(|e| e.into())
    }

    pub async fn find_task(&self, id: Uuid) -> Result<Option<Task>> {
        let mut tx = self.pool.begin().await?;

        // non-macro query_as used because of sqlx limitations with enums.
        let task = sqlx::query_as::<_, Task>("SELECT * FROM task WHERE id = $1")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;

        // Fetch accompanied Files for the Task.
        match task {
            Some(mut task) => {
                let mut files = sqlx::query_as::<_, File>("SELECT * FROM file WHERE task_id = $1")
                    .bind(id)
                    .fetch_all(&mut *tx)
                    .await?;
                task.files.append(&mut files);
                Ok(Some(task))
            }
            None => Ok(None),
        }
    }

    pub async fn get_tasks(&self) -> Result<Vec<Task>> {
        let mut tx = self.pool.begin().await?;

        // non-macro query_as used because of sqlx limitations with enums.
        let mut tasks = sqlx::query_as::<_, Task>("SELECT * FROM task")
            .fetch_all(&mut *tx)
            .await?;

        for task in &mut tasks {
            let mut files = sqlx::query_as::<_, File>("SELECT * FROM file WHERE task_id = $1")
                .bind(task.id)
                .fetch_all(&mut *tx)
                .await?;

            task.files.append(&mut files);
        }

        Ok(tasks)
    }

    pub async fn update_task_state(&self, t: &Task) -> Result<()> {
        sqlx::query("UPDATE task SET state = $1 WHERE id = $2")
            .bind(&t.state)
            .bind(t.id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn add_asset(&self, tx_hash: &Hash) -> Result<()> {
        sqlx::query!(
            "INSERT INTO assets ( tx ) VALUES ( $1 ) RETURNING *",
            tx_hash.to_string(),
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn get_incomplete_assets(&self) -> Result<Vec<Hash>> {
        let assets =
            sqlx::query("SELECT tx FROM assets WHERE completed IS NULL ORDER BY created ASC")
                .map(|row: sqlx::postgres::PgRow| row.get(0))
                .fetch_all(&self.pool)
                .await?;

        Ok(assets)
    }

    pub async fn mark_asset_complete(&self, tx_hash: &Hash) -> Result<()> {
        sqlx::query("UPDATE assets SET complete = NOW() WHERE tx = $1")
            .bind(&tx_hash.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    // NOTE: There are plenty of opportunities for optimizations in following
    // transaction related operations. They are implemented naively on purpose
    // for now to maintain initial flexibility in development. Later on, these
    // queries here are easy low hanging fruits for optimizations.
    pub async fn find_transaction(
        &self,
        tx_hash: impl AsRef<Hash>,
    ) -> Result<Option<types::Transaction>> {
        let mut db_tx = self.pool.begin().await?;

        let entity =
            sqlx::query_as::<_, entity::Transaction>("SELECT * FROM transaction WHERE hash = $1")
                .bind(tx_hash.as_ref())
                .fetch_optional(&mut *db_tx)
                .await?;

        if entity.is_some() {
            let entity = entity.unwrap();
            let payload = match entity.kind {
                entity::transaction::Kind::Deploy => {
                    let deploy = sqlx::query_as::<_, entity::payload::Deploy>(
                        "SELECT * FROM deploy WHERE tx = $1",
                    )
                    .bind(tx_hash.as_ref())
                    .fetch_one(&mut *db_tx)
                    .await?;

                    let prover = self.get_program(&mut db_tx, deploy.prover).await?;
                    let verifier = self.get_program(&mut db_tx, deploy.verifier).await?;

                    types::transaction::Payload::Deploy {
                        name: deploy.name,
                        prover: prover.into(),
                        verifier: verifier.into(),
                    }
                }
                entity::transaction::Kind::Run => {
                    let steps = sqlx::query_as::<_, entity::payload::WorkflowStep>(
                        "SELECT * FROM workflow_step WHERE tx = $1",
                    )
                    .bind(tx_hash.as_ref())
                    .fetch_all(&mut *db_tx)
                    .await?;

                    let program_inputs = sqlx::query_as::<_, entity::payload::ProgramInputData>(
                        "SELECT * FROM program_input_data AS pid JOIN workflow_step AS ws ON pid.workflow_step_id = ws.id WHERE ws.tx = $1",
                    )
                    .bind(tx_hash.as_ref())
                    .fetch_all(&mut *db_tx)
                    .await?;

                    let program_outputs = sqlx::query_as::<_, entity::payload::ProgramOutputData>(
                        "SELECT * FROM program_output_data AS pod JOIN workflow_step AS ws ON pod.workflow_step_id = ws.id WHERE ws.tx = $1",
                    )
                    .bind(&tx_hash.as_ref())
                    .fetch_all(&mut *db_tx)
                    .await?;

                    let steps = steps
                        .iter()
                        .map(|step| {
                            let step_id = step.id.unwrap();
                            let program_inputs: Vec<&entity::payload::ProgramInputData> =
                                program_inputs
                                    .iter()
                                    .filter(|e| e.workflow_step_id == step_id)
                                    .collect();
                            let program_outputs: Vec<&entity::payload::ProgramOutputData> =
                                program_outputs
                                    .iter()
                                    .filter(|e| e.workflow_step_id == step_id)
                                    .collect();

                            let mut program_data: Vec<types::transaction::ProgramData> =
                                program_inputs
                                    .iter()
                                    .map(|e| types::transaction::ProgramData::Input {
                                        file_name: e.file_name.clone(),
                                        file_url: e.file_url.clone(),
                                        checksum: e.checksum.clone(),
                                    })
                                    .collect();

                            let mut program_outputs: Vec<types::transaction::ProgramData> =
                                program_outputs
                                    .iter()
                                    .map(|e| types::transaction::ProgramData::Output {
                                        file_name: e.file_name.clone(),
                                        source_program: e.source_program,
                                    })
                                    .collect();
                            program_data.append(&mut program_outputs);

                            types::transaction::WorkflowStep {
                                program: step.program,
                                args: step.args.clone(),
                                inputs: program_data,
                            }
                        })
                        .collect();

                    types::transaction::Payload::Run {
                        workflow: types::transaction::Workflow { steps },
                    }
                }
                _ => types::transaction::Payload::Empty,
            };

            let mut tx: types::transaction::Transaction = entity.into();
            tx.payload = payload;
            Ok(Some(tx))
        } else {
            Ok(None)
        }
    }

    pub async fn get_transactions(&self) -> Result<Vec<types::Transaction>> {
        let mut db_tx = self.pool.begin().await?;
        let refs: Vec<Hash> = sqlx::query("SELECT hash FROM transaction")
            .map(|row: sqlx::postgres::PgRow| row.get(0))
            .fetch_all(&mut *db_tx)
            .await?;

        let mut txs = Vec::with_capacity(refs.len());
        for tx_hash in refs {
            let tx = self.find_transaction(tx_hash).await?;
            if tx.is_some() {
                txs.push(tx.unwrap());
            }
        }

        Ok(txs)
    }

    pub async fn add_transaction(&self, tx: &types::Transaction) -> Result<()> {
        let entity = entity::Transaction::from(tx);

        let mut db_tx = self.pool.begin().await?;

        sqlx::query(
            "INSERT INTO transaction ( hash, kind, nonce, signature, propagated ) VALUES ( $1, $2, $3, $4, $5 ) RETURNING *")
            .bind(entity.hash)
            .bind(entity.kind)
            .bind(entity.nonce)
            .bind(entity.signature)
            .bind(entity.propagated)
        .fetch_one(&mut *db_tx)
        .await?;

        match tx.payload {
            types::transaction::Payload::Deploy {
                ref name,
                ref prover,
                ref verifier,
            } => {
                self.add_program(&mut db_tx, &Program::from(prover.clone()))
                    .await?;
                self.add_program(&mut db_tx, &Program::from(verifier.clone()))
                    .await?;

                sqlx::query(
                    "INSERT INTO deploy ( tx, name, prover, verifier ) VALUES ( $1, $2, $3, $4 ) RETURNING *")
                    .bind(tx.hash)
                    .bind(name)
                    .bind(prover.hash)
                    .bind(verifier.hash)
                .fetch_one(&mut *db_tx)
                .await?;
            }
            types::transaction::Payload::Run { ref workflow } => {
                let mut step_sequence = 1;
                for step in &workflow.steps {
                    let result = sqlx::query(
                        "INSERT INTO workflow_step ( tx, sequence, program, args ) VALUES ( $1, $2, $3, $4 ) RETURNING id")
                        .bind(tx.hash)
                        .bind(step_sequence)
                        .bind(step.program)
                        .bind(&step.args)
                    .fetch_one(&mut *db_tx)
                    .await?;

                    let step_id: i64 = result.get(0);

                    for input in &step.inputs {
                        match input {
                            ProgramData::Input {
                                file_name,
                                file_url,
                                checksum,
                            } => {
                                sqlx::query(
                                    "INSERT INTO program_input_data ( workflow_step_id, file_name, file_url, checksum ) VALUES ( $1, $2, $3, $4 ) RETURNING *")
                                    .bind(step_id)
                                    .bind(file_name)
                                    .bind(file_url)
                                    .bind(checksum)
                                .fetch_one(&mut *db_tx)
                                .await?;
                            }
                            ProgramData::Output {
                                file_name,
                                source_program,
                            } => {
                                sqlx::query(
                                    "INSERT INTO program_output_data ( workflow_step_id, file_name, source_program ) VALUES ( $1, $2, $3 ) RETURNING *")
                                    .bind(step_id)
                                    .bind(file_name)
                                    .bind(source_program)
                                .fetch_one(&mut *db_tx)
                                .await?;
                            }
                        }
                    }

                    step_sequence += 1;
                }
            }
            _ => { /* ignore for now */ }
        }

        db_tx.commit().await.map_err(|e| e.into())
    }

    // Delete is mainly for test cases.
    async fn delete_transaction(&self, tx_hash: &Hash) -> Result<()> {
        let mut db_tx = self.pool.begin().await?;

        sqlx::query("DELETE FROM program USING deploy WHERE (program.hash = deploy.prover OR program.hash = deploy.verifier) AND deploy.tx = $1")
            .bind(&tx_hash)
            .execute(&mut *db_tx)
            .await?;
        sqlx::query("DELETE FROM transaction WHERE hash = $1")
            .bind(&tx_hash)
            .execute(&mut *db_tx)
            .await?;

        db_tx.commit().await.map_err(|e| e.into())
    }
}

#[cfg(test)]
mod tests {
    use crate::types::{
        transaction::{Payload, ProgramMetadata},
        Signature, Transaction,
    };

    use super::*;

    #[ignore]
    #[tokio::test]
    async fn test_add_and_find_deploy_transaction() {
        let database = Database::new("postgres://gevulot:gevulot@localhost/gevulot")
            .await
            .expect("failed to connect to db");

        let tx = Transaction {
            hash: Hash::default(),
            payload: Payload::Deploy {
                name: "test deployment".to_string(),
                prover: ProgramMetadata {
                    name: "test prover".to_string(),
                    hash: Hash::from(
                        "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                    ),
                    image_file_name: "test_prover.img".to_string(),
                    image_file_url: "http://example.localhost:8080/foobar/test_prover.img"
                        .to_string(),
                    image_file_checksum:
                        "ebc81c06a5ae263d0d4e4efcb06e668b3b786ccc83cb738de5aabb9b966668db"
                            .to_string(),
                },
                verifier: ProgramMetadata {
                    name: "test verifier".to_string(),
                    hash: Hash::from(
                        "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210",
                    ),
                    image_file_name: "test_verifier.img".to_string(),
                    image_file_url: "http://example.localhost:8080/foobar/test_verifier.img"
                        .to_string(),
                    image_file_checksum:
                        "ebc81c06a5ae263d0d4e4efcb06e668b3b786ccc83cb738de5aabb9b966668aa"
                            .to_string(),
                },
            },
            nonce: 64,
            signature: Signature::default(),
            propagated: false,
        };

        database
            .add_transaction(&tx)
            .await
            .expect("add transaction to db");

        let read_tx = database.find_transaction(&tx.hash).await;

        // Cleanup
        database
            .delete_transaction(&tx.hash)
            .await
            .expect("delete transaction");

        // Assertions
        assert!(read_tx.is_ok());
        let read_tx = read_tx.unwrap();

        assert!(read_tx.is_some());
        assert_eq!(tx, read_tx.unwrap());
    }
}
