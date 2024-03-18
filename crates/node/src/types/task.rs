use super::hash::Hash;
use crate::types::file::{TaskVmFile, VmInput};
use uuid::Uuid;

pub type TaskId = Uuid;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum TaskState {
    #[default]
    New,
    Pending,
    Running,
    Ready,
    Failed,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum TaskKind {
    Proof,
    Verification,
    PoW,
    #[default]
    Nop,
}

#[derive(Clone, Debug, Default)]
pub struct Task {
    pub id: TaskId,
    pub tx: Hash,
    pub name: String,
    pub kind: TaskKind,
    pub program_id: Hash,
    pub args: Vec<String>,
    pub files: Vec<TaskVmFile<VmInput>>,
    pub serial: i32,
    pub state: TaskState,
}

pub struct TaskResult {}
