mod program_manager;
mod resource_manager;

use crate::storage::Database;
use crate::types::{TaskId, TaskState};
use crate::vmm::vm_server::grpc;
use crate::vmm::{vm_server::TaskManager, VMId};
use crate::workflow::{WorkflowEngine, WorkflowError};
use async_trait::async_trait;
pub use program_manager::ProgramManager;
pub use resource_manager::ResourceManager;

use std::time::Instant;
use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
    time::Duration,
};

use eyre::Result;
use tokio::{
    sync::{Mutex, RwLock},
    time::sleep,
};

use crate::{
    mempool::Mempool,
    types::{Hash, Task},
};

use self::program_manager::{ProgramError, ProgramHandle};
use self::resource_manager::ResourceError;

struct RunningTask {
    task_id: TaskId,
    vm_id: Arc<dyn VMId>,
    task_scheduled: Instant,
    task_started: Instant,
}

pub struct Scheduler {
    mempool: Arc<RwLock<Mempool>>,
    database: Arc<Database>,
    program_manager: Mutex<ProgramManager>,
    workflow_engine: Arc<WorkflowEngine>,

    pending_programs: Arc<Mutex<VecDeque<Hash>>>,
    running_tasks: Arc<Mutex<Vec<RunningTask>>>,
    running_vms: Arc<Mutex<Vec<ProgramHandle>>>,
    #[allow(clippy::type_complexity)]
    task_queue: Arc<Mutex<HashMap<Hash, VecDeque<(Task, Instant)>>>>,
}

impl Scheduler {
    pub fn new(
        mempool: Arc<RwLock<Mempool>>,
        database: Arc<Database>,
        program_manager: ProgramManager,
        workflow_engine: Arc<WorkflowEngine>,
    ) -> Self {
        Self {
            mempool,
            database,
            program_manager: Mutex::new(program_manager),
            workflow_engine,

            pending_programs: Arc::new(Mutex::new(VecDeque::new())),
            running_tasks: Arc::new(Mutex::new(vec![])),
            running_vms: Arc::new(Mutex::new(vec![])),
            task_queue: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn run(&self) -> Result<()> {
        'SCHEDULING_LOOP: loop {
            // Before scheduling new workload, try to start pending programs first.
            {
                let mut pending_programs = self.pending_programs.lock().await;
                while let Some(program_id) = pending_programs.pop_front() {
                    match self
                        .program_manager
                        .lock()
                        .await
                        .start_program(program_id, None)
                        .await
                    {
                        Ok(p) => self.running_vms.lock().await.push(p),
                        Err(e) if e.is::<ResourceError>() => {
                            let err = e.downcast_ref::<ResourceError>().unwrap();
                            tracing::info!("resources unavailable: {}", err);
                            sleep(Duration::from_millis(500)).await;
                            continue 'SCHEDULING_LOOP;
                        }
                        Err(e) => panic!("failed to start program: {e}"),
                    }
                }
            }

            let mut task = match self.pick_task().await {
                Some(t) => t,
                None => {
                    sleep(Duration::from_millis(100)).await;
                    continue;
                }
            };

            // Push the task into program's work queue.
            {
                let mut task_queue_map = self.task_queue.lock().await;
                if let Some(task_queue) = task_queue_map.get_mut(&task.program_id) {
                    task_queue.push_back((task.clone(), Instant::now()));
                } else {
                    let mut queue = VecDeque::new();
                    queue.push_back((task.clone(), Instant::now()));
                    task_queue_map.insert(task.program_id, queue);
                }
            }

            // Start the program.
            match self
                .program_manager
                .lock()
                .await
                .start_program(task.program_id, None)
                .await
            {
                Ok(p) => self.running_vms.lock().await.push(p),
                Err(ref err) => {
                    if let Some(err) = err.downcast_ref::<ResourceError>() {
                        let ResourceError::NotEnoughResources(msg) = err;
                        self.reschedule(&task).await?;
                        tracing::warn!("task {} rescheduled: {}", task.id.to_string(), msg);
                        continue;
                    }

                    if let Some(err) = err.downcast_ref::<ProgramError>() {
                        let ProgramError::ProgramNotFound(msg) = err;
                        tracing::error!("failed to schedule task {}: {}", task.id.to_string(), msg);

                        // TODO: Persist failed task state.
                        task.state = TaskState::Failed;

                        // Drop the program's task queue. The program's not
                        // there so no need to have queue for it either.
                        let program_id = &task.program_id.clone();
                        self.task_queue.lock().await.remove(program_id);
                    }
                }
            }
        }
    }

    async fn pick_task(&self) -> Option<Task> {
        // Acquire write lock.
        let mut mempool = self.mempool.write().await;

        // Check if next tx is ready for processing?
        let tx = match mempool.peek() {
            Some(tx) => {
                if self.database.has_assets_loaded(&tx.hash).await.unwrap() {
                    mempool.next().unwrap()
                } else {
                    // Assets are still downloading.
                    // TODO: This can stall the whole processing pipeline!!
                    // XXX: ....^.........^........^......^.......^........
                    return None;
                }
            }
            None => return None,
        };

        match self.workflow_engine.next_task(&tx).await {
            Ok(res) => res,
            Err(e) if e.is::<WorkflowError>() => {
                let err = e.downcast_ref::<WorkflowError>();
                match err {
                    Some(WorkflowError::IncompatibleTransaction(_)) => None,
                    _ => {
                        tracing::error!("failed to compute next task for tx:{}: {}", tx.hash, e);
                        None
                    }
                }
            }
            Err(e) => {
                tracing::error!("failed to compute next task for tx:{}: {}", tx.hash, e);
                None
            }
        }
    }

    async fn reschedule(&self, task: &Task) -> Result<()> {
        // The task is already pending in program's work queue. Push program ID
        // to pending programs queue to wait available resources.
        self.pending_programs
            .lock()
            .await
            .push_back(task.program_id);
        Ok(())
    }
}

#[async_trait]
impl TaskManager for Scheduler {
    async fn get_task(&self, program: Hash, vm_id: Arc<dyn VMId>) -> Option<grpc::Task> {
        if let Some(task_queue) = self.task_queue.lock().await.get(&program) {
            if let Some((task, scheduled)) = task_queue.front() {
                tracing::info!(
                    "task {} started in {}ms",
                    task.id.to_string(),
                    scheduled.elapsed().as_millis()
                );

                self.running_tasks.lock().await.push(RunningTask {
                    task_id: task.id,
                    vm_id: vm_id.clone(),
                    task_scheduled: *scheduled,
                    task_started: Instant::now(),
                });

                return Some(grpc::Task {
                    id: task.id.to_string(),
                    name: task.name.clone(),
                    args: task.args.clone(),
                    files: task.files.iter().map(|x| x.name.clone()).collect(),
                });
            }
        }

        None
    }

    async fn submit_result(
        &self,
        _program: Hash,
        _vm_id: Arc<dyn VMId>,
        result: grpc::task_result_request::Result,
    ) -> bool {
        dbg!(&result);

        let grpc::task_result_request::Result::Task(result) = result else {
            todo!("task failed; handle it correctly")
        };

        let task_id = TaskId::parse_str(&result.id).unwrap();
        let mut running_tasks = self.running_tasks.lock().await;
        if let Some(idx) = running_tasks.iter().position(|e| e.task_id == task_id) {
            let running_task = running_tasks.swap_remove(idx);
            tracing::info!(
                "task {} finished in {}sec",
                task_id.to_string(),
                running_task.task_started.elapsed().as_secs()
            );
        }

        return false;
    }
}
