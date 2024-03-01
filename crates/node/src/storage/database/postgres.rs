use super::entity::{self};
use crate::txvalidation::acl::AclWhiteListError;
use crate::txvalidation::acl::AclWhitelist;
use crate::types::file::DbFile;
use crate::types::{
    self,
    transaction::{ProgramData, Validated},
    Hash, Program,
};
use eyre::Result;
use gevulot_node::types::program::ResourceRequest;
use libsecp256k1::PublicKey;
use sqlx::{postgres::PgPoolOptions, FromRow, Row};
use std::time::Duration;

const MAX_DB_CONNS: u32 = 64;
const DB_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

#[async_trait::async_trait]
impl AclWhitelist for Database {
    async fn contains(&self, key: &PublicKey) -> Result<bool, AclWhiteListError> {
        let key = entity::PublicKey(*key);
        self.acl_whitelist_has(&key).await.map_err(|err| {
            AclWhiteListError::InternalError(format!("Fail to query access list: {err}",))
        })
    }
}

#[derive(Clone)]
pub struct Database {
    pool: sqlx::PgPool,
}

// TODO: Split this into domain specific components.
impl Database {
    pub async fn new(db_url: &str) -> Result<Database> {
        let pool = PgPoolOptions::new()
            .max_connections(MAX_DB_CONNS)
            .acquire_timeout(DB_CONNECT_TIMEOUT)
            .connect(db_url)
            .await?;
        Ok(Database { pool })
    }

    pub async fn add_program(&self, db_conn: &mut sqlx::PgConnection, p: &Program) -> Result<()> {
        let mut db_tx = self.pool.begin().await?;

        sqlx::query(
            "INSERT INTO program ( hash, name, image_file_name, image_file_url, image_file_checksum ) VALUES ( $1, $2, $3, $4, $5 ) ON CONFLICT (hash) DO NOTHING")
            .bind(p.hash)
            .bind(&p.name)
            .bind(&p.image_file_name)
            .bind(&p.image_file_url)
            .bind(&p.image_file_checksum)
        .execute(&mut *db_tx)
        .await?;

        if let Some(ref program_resource_requirements) = p.limits {
            sqlx::query("INSERT INTO program_resource_requirements ( program_hash, memory, cpus, gpus ) VALUES ( $1, $2, $3, $4 ) ON CONFLICT (program_hash) DO NOTHING")
                .bind(p.hash)
                .bind(program_resource_requirements.mem as i64)
                .bind(program_resource_requirements.cpus as i64)
                .bind(program_resource_requirements.gpus as i64)
            .execute(&mut *db_tx)
            .await?;
        }

        db_tx.commit().await.map_err(|e| e.into())
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
        let program = sqlx::query("SELECT * FROM program LEFT JOIN program_resource_requirements AS prr ON prr.program_hash = program.hash WHERE hash = $1")
            .bind(hash.as_ref())
            .try_map(|row: sqlx::postgres::PgRow| {
                let mut prg = Program::from_row(&row)?;
                prg.limits = match ResourceRequest::from_row(&row) {
                    Ok(rr) => Some(rr),
                    Err(_) => {
                        // If program doesn't have specific entry for resource
                        // requirements, the fields are NULL, which causes an
                        // error due to lack of Option<> in `ResourceRequest`.
                        // This is a compromise between sound core data types
                        // in the program & perfect mapping with the DB.
                        None
                    }
                };

                Ok(prg)
            })
            .fetch_one(db_conn)
            .await?;

        Ok(program)
    }

    pub async fn get_programs(&self) -> Result<Vec<Program>> {
        let programs = sqlx::query("SELECT * FROM program LEFT JOIN program_resource_requirements AS prr ON prr.program_hash = program.hash")
            .try_map(|row: sqlx::postgres::PgRow| {
                let mut prg = Program::from_row(&row)?;
                prg.limits = match ResourceRequest::from_row(&row) {
                    Ok(rr) => Some(rr),
                    Err(err) => {
                        // If program doesn't have specific entry for resource
                        // requirements, the fields are NULL, which causes an
                        // error due to lack of Option<> in `ResourceRequest`.
                        // This is a compromise between sound core data types
                        // in the program & perfect mapping with the DB.
                        None
                    }
                };

                Ok(prg)
            })
            .fetch_all(&self.pool)
            .await?;

        Ok(programs)
    }

    // NOTE: There are plenty of opportunities for optimizations in following
    // transaction related operations. They are implemented naively on purpose
    // for now to maintain initial flexibility in development. Later on, these
    // queries here are easy low hanging fruits for optimizations.
    pub async fn find_transaction(
        &self,
        tx_hash: &Hash,
    ) -> Result<Option<types::Transaction<Validated>>> {
        println!("find_transaction");

        let mut db_tx = self.pool.begin().await?;

        let entity =
            sqlx::query_as::<_, entity::Transaction>("SELECT * FROM transaction WHERE hash = $1")
                .bind(tx_hash)
                .fetch_optional(&mut *db_tx)
                .await?;

        if entity.is_some() {
            let entity = entity.unwrap();
            let payload = match entity.kind {
                entity::transaction::Kind::Deploy => {
                    let deploy = sqlx::query_as::<_, entity::payload::Deploy>(
                        "SELECT * FROM deploy WHERE tx = $1",
                    )
                    .bind(tx_hash)
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
                    .bind(tx_hash)
                    .fetch_all(&mut *db_tx)
                    .await?;

                    let program_inputs = sqlx::query_as::<_, entity::payload::ProgramInputData>(
                        "SELECT * FROM program_input_data AS pid JOIN workflow_step AS ws ON pid.workflow_step_id = ws.id WHERE ws.tx = $1",
                    )
                    .bind(tx_hash)
                    .fetch_all(&mut *db_tx)
                    .await?;

                    let program_outputs = sqlx::query_as::<_, entity::payload::ProgramOutputData>(
                        "SELECT * FROM program_output_data AS pod JOIN workflow_step AS ws ON pod.workflow_step_id = ws.id WHERE ws.tx = $1",
                    )
                    .bind(tx_hash)
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
                entity::transaction::Kind::Proof => {
                    //get payload files
                    let files =
                        sqlx::query_as::<_, DbFile>("SELECT * FROM txfile WHERE tx_id = $1")
                            .bind(tx_hash)
                            .fetch_all(&mut *db_tx)
                            .await?;

                    sqlx::query("SELECT parent, prover, proof FROM proof WHERE tx = $1")
                        .bind(tx_hash)
                        .map(
                            |row: sqlx::postgres::PgRow| types::transaction::Payload::Proof {
                                parent: row.get(0),
                                prover: row.get(1),
                                proof: row.get(2),
                                files: files.clone().into_iter().map(|file| file.into()).collect(),
                            },
                        )
                        .fetch_one(&mut *db_tx)
                        .await?
                }
                entity::transaction::Kind::ProofKey => {
                    sqlx::query("SELECT parent, key FROM proof_key WHERE tx = $1")
                        .bind(tx_hash)
                        .map(
                            |row: sqlx::postgres::PgRow| types::transaction::Payload::ProofKey {
                                parent: row.get(0),
                                key: row.get(1),
                            },
                        )
                        .fetch_one(&mut *db_tx)
                        .await?
                }
                entity::transaction::Kind::Verification => {
                    //get payload files
                    let files =
                        sqlx::query_as::<_, DbFile>("SELECT * FROM txfile WHERE tx_id = $1")
                            .bind(tx_hash)
                            .fetch_all(&mut *db_tx)
                            .await?;
                    sqlx::query(
                        "SELECT parent, verifier, verification FROM verification WHERE tx = $1",
                    )
                    .bind(tx_hash)
                    .map(
                        |row: sqlx::postgres::PgRow| types::transaction::Payload::Verification {
                            parent: row.get(0),
                            verifier: row.get(1),
                            verification: row.get(2),
                            files: files.clone().into_iter().map(|file| file.into()).collect(),
                        },
                    )
                    .fetch_one(&mut *db_tx)
                    .await?
                }
                _ => types::transaction::Payload::Empty,
            };

            let mut tx: types::transaction::Transaction<Validated> = entity.into();
            tx.payload = payload;
            Ok(Some(tx))
        } else {
            Ok(None)
        }
    }

    pub async fn get_transactions(&self) -> Result<Vec<types::Transaction<Validated>>> {
        let mut db_tx = self.pool.begin().await?;
        let refs: Vec<Hash> = sqlx::query("SELECT hash FROM transaction")
            .map(|row: sqlx::postgres::PgRow| row.get(0))
            .fetch_all(&mut *db_tx)
            .await?;

        let mut txs = Vec::with_capacity(refs.len());
        for tx_hash in refs {
            let tx = self.find_transaction(&tx_hash).await?;
            if let Some(tx) = tx {
                txs.push(tx);
            }
        }

        Ok(txs)
    }

    pub async fn get_unexecuted_transactions(&self) -> Result<Vec<types::Transaction<Validated>>> {
        let mut db_tx = self.pool.begin().await?;
        let refs: Vec<Hash> = sqlx::query("SELECT hash FROM transaction WHERE executed IS false")
            .map(|row: sqlx::postgres::PgRow| row.get(0))
            .fetch_all(&mut *db_tx)
            .await?;

        let mut txs = Vec::with_capacity(refs.len());
        for tx_hash in refs {
            let tx = self.find_transaction(&tx_hash).await?;
            if let Some(tx) = tx {
                txs.push(tx);
            }
        }

        Ok(txs)
    }

    pub async fn get_transaction_tree(&self, tx_hash: &Hash) -> Result<Vec<(Hash, Option<Hash>)>> {
        let refs: Vec<(Hash, Option<Hash>)> = sqlx::query("
            SELECT t.hash, NULL FROM transaction AS t JOIN proof AS p ON t.hash = p.parent JOIN verification AS v ON (t.hash = p.parent AND p.tx = v.parent) WHERE t.hash = $1 OR p.tx = $1 OR v.tx = $1
        UNION
            SELECT t.hash, p.parent FROM transaction AS t JOIN proof AS p on t.hash = p.tx WHERE p.parent IN (SELECT t.hash FROM transaction AS t JOIN proof AS p ON t.hash = p.parent JOIN verification AS v ON (t.hash = p.parent AND p.tx = v.parent) WHERE t.hash = $1 OR p.tx = $1 OR v.tx = $1)
        UNION
            SELECT t.hash, v.parent FROM transaction AS t JOIN verification AS v on t.hash = v.tx WHERE v.parent IN (SELECT p.tx FROM proof AS p WHERE p.parent IN (SELECT t.hash FROM transaction AS t JOIN proof AS p ON t.hash = p.parent JOIN verification AS v ON (t.hash = p.parent AND p.tx = v.parent) WHERE t.hash = $1 OR p.tx = $1 OR v.tx = $1))
        ")
            .bind(tx_hash)
            .map(|row: sqlx::postgres::PgRow| (row.get(0), row.get(1)))
            .fetch_all(&self.pool)
            .await?;

        Ok(refs)
    }

    pub async fn add_transaction(&self, tx: &types::Transaction<Validated>) -> Result<()> {
        let entity = entity::Transaction::from(tx);

        let mut db_tx = self.pool.begin().await?;

        sqlx::query(
            "INSERT INTO transaction ( author, hash, kind, nonce, signature, propagated, executed ) VALUES ( $1, $2, $3, $4, $5, $6, $7 ) ON CONFLICT (hash) DO UPDATE SET propagated = $6, executed = $7")
            .bind(entity.author)
            .bind(entity.hash)
            .bind(entity.kind)
            .bind(entity.nonce)
            .bind(entity.signature)
            .bind(entity.propagated)
            .bind(entity.executed)
        .execute(&mut *db_tx)
        .await?;

        match &tx.payload {
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
                    "INSERT INTO deploy ( tx, name, prover, verifier ) VALUES ( $1, $2, $3, $4 ) ON CONFLICT (tx) DO NOTHING")
                    .bind(tx.hash)
                    .bind(name)
                    .bind(prover.hash)
                    .bind(verifier.hash)
                .execute(&mut *db_tx)
                .await?;
            }
            types::transaction::Payload::Run { ref workflow } => {
                let mut step_sequence = 1;
                for step in &workflow.steps {
                    let result = sqlx::query(
                        "WITH ws AS (INSERT INTO workflow_step ( tx, sequence, program, args ) VALUES ( $1, $2, $3, $4 ) ON CONFLICT (tx, sequence) DO NOTHING RETURNING id) SELECT * FROM ws UNION SELECT id FROM workflow_step WHERE tx = $1 AND sequence = $2")
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
                                    "INSERT INTO program_input_data ( workflow_step_id, file_name, file_url, checksum ) VALUES ( $1, $2, $3, $4 ) ON CONFLICT (workflow_step_id, file_name) DO NOTHING")
                                    .bind(step_id)
                                    .bind(file_name)
                                    .bind(file_url)
                                    .bind(checksum)
                                .execute(&mut *db_tx)
                                .await?;
                            }
                            ProgramData::Output {
                                file_name,
                                source_program,
                            } => {
                                sqlx::query(
                                    "INSERT INTO program_output_data ( workflow_step_id, file_name, source_program ) VALUES ( $1, $2, $3 ) ON CONFLICT (workflow_step_id, file_name) DO NOTHING")
                                    .bind(step_id)
                                    .bind(file_name)
                                    .bind(source_program)
                                .execute(&mut *db_tx)
                                .await?;
                            }
                        }
                    }

                    step_sequence += 1;
                }
            }
            types::transaction::Payload::Proof {
                parent,
                prover,
                proof,
                files,
            } => {
                sqlx::query(
                    "INSERT INTO proof ( tx, parent, prover, proof ) VALUES ( $1, $2, $3, $4 ) ON CONFLICT (tx) DO NOTHING",
                )
                .bind(tx.hash)
                .bind(parent)
                .bind(prover)
                .bind(proof)
                .execute(&mut *db_tx)
                .await?;

                //save payload files
                if !files.is_empty() {
                    let mut query_builder = sqlx::QueryBuilder::new(
                        "INSERT INTO txfile ( tx_id, name, url, checksum )",
                    );
                    query_builder.push_values(files, |mut b, new_file| {
                        b.push_bind(tx.hash)
                            .push_bind(&new_file.name)
                            .push_bind(&new_file.url)
                            .push_bind(new_file.checksum);
                    });

                    let query = query_builder.build();
                    if let Err(err) = query.execute(&mut *db_tx).await {
                        db_tx.rollback().await?;
                        return Err(err.into());
                    }
                }
            }

            types::transaction::Payload::ProofKey { parent, key } => {
                sqlx::query("INSERT INTO proof_key ( tx, parent, key ) VALUES ( $1, $2, $3 ) ON CONFLICT (tx) DO NOTHING")
                    .bind(tx.hash)
                    .bind(parent)
                    .bind(key)
                    .execute(&mut *db_tx)
                    .await?;
            }

            types::transaction::Payload::Verification {
                parent,
                verifier,
                verification,
                files,
            } => {
                tracing::trace!("Postgres add_transaction tx:{}", tx.hash.to_string());
                sqlx::query(
                    "INSERT INTO verification ( tx, parent, verifier, verification ) VALUES ( $1, $2, $3, $4 ) ON CONFLICT (tx) DO NOTHING",
                )
                .bind(tx.hash)
                .bind(parent)
                .bind(verifier)
                .bind(verification)
                .execute(&mut *db_tx)
                .await?;

                //save payload files
                if !files.is_empty() {
                    let mut query_builder = sqlx::QueryBuilder::new(
                        "INSERT INTO txfile ( tx_id, name, url, checksum )",
                    );
                    query_builder.push_values(files, |mut b, new_file| {
                        b.push_bind(tx.hash)
                            .push_bind(&new_file.name)
                            .push_bind(&new_file.url)
                            .push_bind(new_file.checksum);
                    });

                    let query = query_builder.build();
                    if let Err(err) = query.execute(&mut *db_tx).await {
                        db_tx.rollback().await?;
                        return Err(err.into());
                    }
                }
            }
            _ => { /* ignore for now */ }
        }

        db_tx.commit().await.map_err(|e| e.into())
    }

    pub async fn mark_tx_executed(&self, tx_hash: &Hash) -> Result<()> {
        sqlx::query("UPDATE transaction SET executed = true WHERE hash = $1")
            .bind(tx_hash)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn acl_whitelist_has(&self, key: &entity::PublicKey) -> Result<bool> {
        let res: Option<i32> = sqlx::query("SELECT 1 FROM acl_whitelist WHERE key = $1")
            .bind(key)
            .map(|row: sqlx::postgres::PgRow| row.get(0))
            .fetch_optional(&self.pool)
            .await?;

        Ok(res.is_some())
    }

    pub async fn acl_whitelist(&self, key: &entity::PublicKey) -> Result<()> {
        sqlx::query("INSERT INTO acl_whitelist ( key ) VALUES ( $1 ) ON CONFLICT (key) DO NOTHING")
            .bind(key)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn acl_deny(&self, key: &entity::PublicKey) -> Result<()> {
        sqlx::query("DELETE FROM acl_whitelist WHERE key = $1")
            .bind(key)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    // Delete is mainly for test cases.
    async fn delete_transaction(&self, tx_hash: &Hash) -> Result<()> {
        let mut db_tx = self.pool.begin().await?;

        sqlx::query("DELETE FROM program USING deploy WHERE (program.hash = deploy.prover OR program.hash = deploy.verifier) AND deploy.tx = $1")
            .bind(tx_hash)
            .execute(&mut *db_tx)
            .await?;
        sqlx::query("DELETE FROM transaction WHERE hash = $1")
            .bind(tx_hash)
            .execute(&mut *db_tx)
            .await?;

        db_tx.commit().await.map_err(|e| e.into())
    }

    async fn delete_program(&self, program_hash: &Hash) -> Result<()> {
        sqlx::query("DELETE FROM program WHERE hash = $1")
            .bind(program_hash)
            .execute(&self.pool)
            .await?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::types::transaction::Payload;
    use gevulot_node::types::transaction::Created;
    use libsecp256k1::SecretKey;
    use rand::{rngs::StdRng, Rng, SeedableRng};

    use crate::types::{transaction::ProgramMetadata, Signature, Transaction};

    use super::*;

    //TODO change by impl From when module declaration between main and lib are solved.
    fn into_validated(tx: Transaction<Created>) -> Transaction<Validated> {
        Transaction {
            author: tx.author,
            hash: tx.hash,
            payload: tx.payload,
            nonce: tx.nonce,
            signature: tx.signature,
            propagated: tx.executed,
            executed: tx.executed,
            state: Validated,
        }
    }

    #[ignore]
    #[tokio::test]
    async fn test_add_and_find_deploy_transaction() {
        let database = Database::new("postgres://gevulot:gevulot@localhost/gevulot")
            .await
            .expect("failed to connect to db");

        let tx = Transaction {
            author: PublicKey::from_secret_key(&SecretKey::default()),
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
                    resource_requirements: None,
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
                    resource_requirements: None,
                },
            },
            nonce: 64,
            signature: Signature::default(),
            propagated: false,
            executed: false,
            state: Validated,
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

    #[ignore]
    #[tokio::test]
    async fn test_get_transaction_tree() {
        let rng = &mut StdRng::from_entropy();
        let database = Database::new("postgres://gevulot:gevulot@localhost/gevulot")
            .await
            .expect("failed to connect to db");

        let key = SecretKey::random(rng);
        let prover = Program {
            hash: Hash::random(rng),
            name: String::from("prover"),
            image_file_name: String::from("prover.img"),
            image_file_url: String::from("http://example.com/prover.img"),
            image_file_checksum: String::from("nope"),
            limits: Default::default(),
        };

        let verifier = Program {
            hash: Hash::random(rng),
            name: String::from("verifier"),
            image_file_name: String::from("verifier.img"),
            image_file_url: String::from("http://example.com/verifier.img"),
            image_file_checksum: String::from("nope"),
            limits: Default::default(),
        };

        let run_tx = Transaction::new(
            Payload::Run {
                workflow: types::transaction::Workflow::default(),
            },
            &key,
        );
        let proof1_tx = Transaction::new(
            Payload::Proof {
                parent: run_tx.hash,
                prover: prover.hash,
                proof: vec![1],
                files: vec![],
            },
            &key,
        );
        let proof2_tx = Transaction::new(
            Payload::Proof {
                parent: run_tx.hash,
                prover: prover.hash,
                proof: vec![2],
                files: vec![],
            },
            &key,
        );
        let proof3_tx = Transaction::new(
            Payload::Proof {
                parent: run_tx.hash,
                prover: prover.hash,
                proof: vec![3],
                files: vec![],
            },
            &key,
        );
        let proof4_tx = Transaction::new(
            Payload::Proof {
                parent: run_tx.hash,
                prover: prover.hash,
                proof: vec![4],
                files: vec![],
            },
            &key,
        );

        let verification1_tx = Transaction::new(
            Payload::Verification {
                parent: proof1_tx.hash,
                verifier: verifier.hash,
                verification: vec![1],
                files: vec![],
            },
            &key,
        );
        let verification2_tx = Transaction::new(
            Payload::Verification {
                parent: proof2_tx.hash,
                verifier: verifier.hash,
                verification: vec![2],
                files: vec![],
            },
            &key,
        );
        let verification3_tx = Transaction::new(
            Payload::Verification {
                parent: proof3_tx.hash,
                verifier: verifier.hash,
                verification: vec![3],
                files: vec![],
            },
            &key,
        );
        let verification4_tx = Transaction::new(
            Payload::Verification {
                parent: proof4_tx.hash,
                verifier: verifier.hash,
                verification: vec![4],
                files: vec![],
            },
            &key,
        );

        let txs = vec![
            run_tx,
            proof1_tx,
            proof2_tx,
            proof3_tx,
            proof4_tx,
            verification1_tx,
            verification2_tx,
            verification3_tx,
            verification4_tx,
        ];

        let mut db_conn = database
            .pool
            .acquire()
            .await
            .expect("acquire DB connection from pool");
        database
            .add_program(&mut db_conn, &prover)
            .await
            .expect("add prover");
        database
            .add_program(&mut db_conn, &verifier)
            .await
            .expect("add verifier");

        for tx in &txs {
            database
                .add_transaction(&into_validated(tx.clone()))
                .await
                .expect("add transaction");
        }

        // Pick random transaction from set.
        let random_idx = rng.gen_range(0..txs.len());

        // Fetch the whole tx tree. All transactions should be returned.
        let results = database
            .get_transaction_tree(&txs.get(random_idx).unwrap().hash)
            .await;

        assert!(results.is_ok());

        let mut results = results.unwrap();

        for tx in &txs {
            let idx = results.iter().position(|x| x.0 == tx.hash);
            if idx.is_none() {
                panic!(
                    "Couldn't find transaction {} from get_transaction_tree() results.",
                    tx.hash
                );
            }

            let idx = idx.unwrap();
            results.remove(idx);
        }

        // After all transactions have been processed, no extra ones should be
        // left in results.
        assert!(results.is_empty());

        // Cleanup
        for tx in txs {
            database
                .delete_transaction(&tx.hash)
                .await
                .expect("delete transaction");
        }

        database
            .delete_program(&prover.hash)
            .await
            .expect("delete program");
        database
            .delete_program(&verifier.hash)
            .await
            .expect("delete program");
    }

    #[ignore]
    #[tokio::test]
    async fn test_programs_with_resource_requirements() {
        let rng = &mut StdRng::from_entropy();
        let database = Database::new("postgres://gevulot:gevulot@localhost/gevulot")
            .await
            .expect("failed to connect to db");

        let prg = Program {
            hash: Hash::random(rng),
            name: String::from("prover"),
            image_file_name: String::from("prover.img"),
            image_file_url: String::from("http://example.com/prover.img"),
            image_file_checksum: String::from("nope"),
            limits: Some(ResourceRequest {
                mem: 53912,
                cpus: 13,
                gpus: 3,
            }),
        };

        let mut db_conn = database.pool.acquire().await.expect("acquire db conn");

        database
            .add_program(&mut db_conn, &prg)
            .await
            .expect("add program");

        let p = database
            .get_program(&mut db_conn, prg.hash)
            .await
            .expect("get program");

        // Cleanup before final assertions (that can fail).
        database
            .delete_program(&prg.hash)
            .await
            .expect("delete program");

        assert_eq!(p.hash, prg.hash);
        assert_eq!(p.name, prg.name);
        assert_eq!(p.image_file_name, prg.image_file_name);
        assert_eq!(p.image_file_url, prg.image_file_url);
        assert_eq!(p.image_file_checksum, prg.image_file_checksum);
        assert_eq!(p.limits, prg.limits);
    }

    #[ignore]
    #[tokio::test]
    async fn test_programs_without_resource_requirements() {
        let rng = &mut StdRng::from_entropy();
        let database = Database::new("postgres://gevulot:gevulot@localhost/gevulot")
            .await
            .expect("failed to connect to db");

        let prg = Program {
            hash: Hash::random(rng),
            name: String::from("prover"),
            image_file_name: String::from("prover.img"),
            image_file_url: String::from("http://example.com/prover.img"),
            image_file_checksum: String::from("nope"),
            limits: None,
        };

        let mut db_conn = database.pool.acquire().await.expect("acquire db conn");

        database
            .add_program(&mut db_conn, &prg)
            .await
            .expect("add program");

        let p = database
            .get_program(&mut db_conn, prg.hash)
            .await
            .expect("get program");

        // Cleanup before final assertions (that can fail).
        database
            .delete_program(&prg.hash)
            .await
            .expect("delete program");

        assert_eq!(p.hash, prg.hash);
        assert_eq!(p.name, prg.name);
        assert_eq!(p.image_file_name, prg.image_file_name);
        assert_eq!(p.image_file_url, prg.image_file_url);
        assert_eq!(p.image_file_checksum, prg.image_file_checksum);
        assert_eq!(p.limits, None);
    }

    #[ignore]
    #[tokio::test]
    async fn test_programs_with_and_without_resource_requirements() {
        let rng = &mut StdRng::from_entropy();
        let database = Database::new("postgres://gevulot:gevulot@localhost/gevulot")
            .await
            .expect("failed to connect to db");

        let prg1 = Program {
            hash: Hash::random(rng),
            name: String::from("prover"),
            image_file_name: String::from("prover.img"),
            image_file_url: String::from("http://example.com/prover.img"),
            image_file_checksum: String::from("nope"),
            limits: Some(ResourceRequest {
                mem: 53912,
                cpus: 13,
                gpus: 3,
            }),
        };

        let prg2 = Program {
            hash: Hash::random(rng),
            name: String::from("prover"),
            image_file_name: String::from("prover.img"),
            image_file_url: String::from("http://example.com/prover.img"),
            image_file_checksum: String::from("nope"),
            limits: None,
        };

        let mut db_conn = database.pool.acquire().await.expect("acquire db conn");

        database
            .add_program(&mut db_conn, &prg1)
            .await
            .expect("add program #1");

        database
            .add_program(&mut db_conn, &prg2)
            .await
            .expect("add program #2");

        let programs = database.get_programs().await.expect("get programs");

        // Cleanup before final assertions (that can fail).
        database
            .delete_program(&prg1.hash)
            .await
            .expect("delete program");
        database
            .delete_program(&prg2.hash)
            .await
            .expect("delete program");

        // Assertions.
        for p in programs {
            // Compare program hash for specific assertion path because
            // `programs` will contain artifacts from other tests as well,
            // due to concurrency.
            if p.hash == prg1.hash {
                assert_eq!(p.hash, prg1.hash);
                assert_eq!(p.name, prg1.name);
                assert_eq!(p.image_file_name, prg1.image_file_name);
                assert_eq!(p.image_file_url, prg1.image_file_url);
                assert_eq!(p.image_file_checksum, prg1.image_file_checksum);
                assert_eq!(p.limits, prg1.limits);
            } else if p.hash == prg2.hash {
                assert_eq!(p.hash, prg2.hash);
                assert_eq!(p.name, prg2.name);
                assert_eq!(p.image_file_name, prg2.image_file_name);
                assert_eq!(p.image_file_url, prg2.image_file_url);
                assert_eq!(p.image_file_checksum, prg2.image_file_checksum);
                assert_eq!(p.limits, None);
            }
        }
    }
}
