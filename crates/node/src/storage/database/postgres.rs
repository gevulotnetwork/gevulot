use eyre::Result;
use sqlx::{self, Row};
use uuid::Uuid;

use crate::types::{self, File, Hash, Program, Task};

use super::entity;

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

    pub async fn add_program(&self, p: &Program) -> Result<()> {
        sqlx::query!(
            "INSERT INTO programs ( hash, name, image_file_name, image_file_url, image_file_checksum ) VALUES ( $1, $2, $3, $4, $5 ) RETURNING *",
            p.hash.to_string(),
            p.name,
            p.image_file_name,
            p.image_file_url,
            p.image_file_checksum,
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn find_program(&self, hash: &Hash) -> Result<Option<Program>> {
        // non-macro query_as used because of sqlx limitations with enums.
        let program = sqlx::query_as::<_, Program>("SELECT * FROM programs WHERE hash = $1")
            .bind(hash.to_string())
            .fetch_optional(&self.pool)
            .await?;

        Ok(program)
    }

    pub async fn get_programs(&self) -> Result<Vec<Program>> {
        let programs = sqlx::query_as::<_, Program>("SELECT * FROM programs")
            .fetch_all(&self.pool)
            .await?;
        Ok(programs)
    }

    pub async fn add_task(&self, t: &Task) -> Result<()> {
        let mut tx = self.pool.begin().await?;

        if let Err(err) = sqlx::query(
            "INSERT INTO tasks ( id, name, args, state, program_id ) VALUES ( $1, $2, $3, $4, $5 )",
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
                sqlx::QueryBuilder::new("INSERT INTO files ( task_id, name, url )");
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
        let task = sqlx::query_as::<_, Task>("SELECT * FROM tasks WHERE id = $1")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;

        // Fetch accompanied Files for the Task.
        match task {
            Some(mut task) => {
                let mut files = sqlx::query_as::<_, File>("SELECT * FROM files WHERE task_id = $1")
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
        let mut tasks = sqlx::query_as::<_, Task>("SELECT * FROM tasks")
            .fetch_all(&mut *tx)
            .await?;

        for task in &mut tasks {
            let mut files = sqlx::query_as::<_, File>("SELECT * FROM files WHERE task_id = $1")
                .bind(task.id)
                .fetch_all(&mut *tx)
                .await?;

            task.files.append(&mut files);
        }

        Ok(tasks)
    }

    pub async fn update_task_state(&self, t: &Task) -> Result<()> {
        sqlx::query("UPDATE tasks SET state = $1 WHERE id = $2")
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

    pub async fn find_transaction(&self, tx_hash: &Hash) -> Result<Option<types::Transaction>> {
        let tx =
            sqlx::query_as::<_, entity::Transaction>("SELECT * FROM transactions WHERE hash = $1")
                .bind(&tx_hash.to_string())
                .fetch_optional(&self.pool)
                .await?;
        Ok(tx.map(|tx| tx.into()))
    }
}
