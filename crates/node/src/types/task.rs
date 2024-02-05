use serde::{Deserialize, Serialize};
use std::path::Path;
use std::path::PathBuf;
use uuid::Uuid;

use super::hash::{deserialize_hash_from_json, Hash};

pub type TaskId = Uuid;

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize, sqlx::Type)]
#[sqlx(type_name = "task_state", rename_all = "lowercase")]
pub enum TaskState {
    #[default]
    New,
    Pending,
    Running,
    Ready,
    Failed,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize, sqlx::Type)]
#[sqlx(type_name = "task_kind", rename_all = "lowercase")]
pub enum TaskKind {
    Proof,
    Verification,
    PoW,
    #[default]
    Nop,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize, sqlx::FromRow)]
pub struct File {
    #[serde(skip_serializing, skip_deserializing)]
    pub tx: Hash,
    pub name: String,
    pub url: String,
}

impl File {
    pub fn get_file_relative_path(&self) -> PathBuf {
        let file_name = Path::new(&self.name).file_name().unwrap();
        PathBuf::new().join(self.tx.to_string()).join(file_name)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize, sqlx::FromRow)]
pub struct Task {
    pub id: TaskId,
    pub tx: Hash,
    pub name: String,
    pub kind: TaskKind,
    #[serde(deserialize_with = "deserialize_hash_from_json")]
    pub program_id: Hash,
    pub args: Vec<String>,
    #[sqlx(skip)]
    pub files: Vec<File>,
    #[serde(skip_deserializing)]
    pub serial: i32,
    #[serde(skip_deserializing)]
    pub state: TaskState,
}

pub struct TaskResult {}
