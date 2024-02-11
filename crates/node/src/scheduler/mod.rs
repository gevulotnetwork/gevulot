mod program_manager;
mod resource_manager;

use crate::storage::Database;
use crate::txvalidation::TxEventSender;
use crate::txvalidation::TxResultSender;
use crate::types::file::{move_vmfile, File, ProofVerif, Vm};
use crate::types::TaskState;
use crate::vmm::vm_server::grpc;
use crate::vmm::{vm_server::TaskManager, VMId};
use crate::workflow::{WorkflowEngine, WorkflowError};
use async_trait::async_trait;
use gevulot_node::types::transaction::Payload;
use gevulot_node::types::{TaskKind, Transaction};
use libsecp256k1::SecretKey;
pub use program_manager::ProgramManager;
use rand::RngCore;
pub use resource_manager::ResourceManager;
use std::path::PathBuf;
use uuid::Uuid;

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

// If VM doesn't have running task within `MAX_VM_IDLE_RUN_TIME`, it will be terminated.
const MAX_VM_IDLE_RUN_TIME: Duration = Duration::from_secs(10);

struct RunningTask {
    task: Task,
    vm_id: Arc<dyn VMId>,
    task_scheduled: Instant,
    task_started: Instant,
}

// TODO: I believe `Scheduler` should be rather named `Node` at some point. It
// has started to become as a kind of central place for node logic
// coordination.
//
// It could use some re-structuring in order to not be so convoluted, but
// otherwise it seems to be okayish.
pub struct Scheduler {
    mempool: Arc<RwLock<Mempool>>,
    database: Arc<Database>,
    program_manager: Mutex<ProgramManager>,
    workflow_engine: Arc<WorkflowEngine>,
    node_key: SecretKey,

    pending_programs: Arc<Mutex<VecDeque<Hash>>>,
    running_tasks: Arc<Mutex<Vec<RunningTask>>>,
    running_vms: Arc<Mutex<Vec<ProgramHandle>>>,
    #[allow(clippy::type_complexity)]
    task_queue: Arc<Mutex<HashMap<Hash, VecDeque<(Task, Instant)>>>>,
    data_directory: PathBuf,
    http_download_host: String,
    tx_sender: TxEventSender<TxResultSender>,
}

impl Scheduler {
    pub fn new(
        mempool: Arc<RwLock<Mempool>>,
        database: Arc<Database>,
        program_manager: ProgramManager,
        workflow_engine: Arc<WorkflowEngine>,
        node_key: SecretKey,
        data_directory: PathBuf,
        http_download_host: String,
        tx_sender: TxEventSender<TxResultSender>,
    ) -> Self {
        Self {
            mempool,
            database,
            program_manager: Mutex::new(program_manager),
            workflow_engine,
            node_key,

            pending_programs: Arc::new(Mutex::new(VecDeque::new())),
            running_tasks: Arc::new(Mutex::new(vec![])),
            running_vms: Arc::new(Mutex::new(vec![])),
            task_queue: Arc::new(Mutex::new(HashMap::new())),
            data_directory,
            http_download_host,
            tx_sender,
        }
    }

    pub async fn run(&self) -> Result<()> {
        'SCHEDULING_LOOP: loop {
            // Reap terminated tasks & VMs.
            self.reap_zombies().await;

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

                            // Return the popped program_id back to pending queue.
                            pending_programs.push_front(program_id);
                            continue 'SCHEDULING_LOOP;
                        }
                        Err(e) => panic!("failed to start program: {e}"),
                    }
                }
            }

            let mut task = match self.pick_task().await {
                Some(t) => {
                    tracing::debug!("task {}/{} scheduled for running", t.id, t.tx);
                    t
                }
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

        match mempool.next() {
            Some(tx) => match self.workflow_engine.next_task(&tx).await {
                Ok(res) => res,
                Err(e) if e.is::<WorkflowError>() => {
                    let err = e.downcast_ref::<WorkflowError>();
                    match err {
                        Some(WorkflowError::IncompatibleTransaction(_)) => {
                            tracing::debug!("{}", e);
                            None
                        }
                        _ => {
                            tracing::error!(
                                "workflow error, failed to compute next task for tx:{}: {}",
                                tx.hash,
                                e
                            );
                            None
                        }
                    }
                }
                Err(e) => {
                    tracing::error!("failed to compute next task for tx:{}: {}", tx.hash, e);
                    None
                }
            },
            None => None,
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

    async fn reap_zombies(&self) {
        let mut running_tasks = self.running_tasks.lock().await;
        let mut running_vms = self.running_vms.lock().await;

        let mut zombies = vec![];
        for task in running_tasks.iter() {
            // TODO: Add maximum running time limit for a task here.

            if let Some(vm_handle) = running_vms
                .iter()
                .find(|x| x.vm_id().eq(task.vm_id.clone()))
            {
                tracing::trace!(
                    "inspecting if VM for task id {} is still alive",
                    task.task.id
                );
                match vm_handle.is_alive().await {
                    Ok(true) => {
                        tracing::trace!("VM is still alive.");
                        continue; // All good
                    }
                    Ok(false) => {
                        // VM is not running anymore.
                        tracing::warn!(
                            "VM {} running for task (id: {}, tx hash: {}) has stopped",
                            task.vm_id,
                            task.task.id,
                            task.task.tx
                        );
                        zombies.push((task.task.id, task.vm_id.clone()));
                    }
                    Err(err) => {
                        tracing::error!("failed to query VM state for {} running task (id: {}, tx hash: {}): {}", task.vm_id, task.task.id, task.task.tx, err);
                        zombies.push((task.task.id, task.vm_id.clone()));
                    }
                }
            } else {
                // VM doesn't exist. Shouldn't happen...
                tracing::error!("found task (id: {}, tx hash: {}) running on VM {} but not the corresponding running VM", task.task.id, task.task.tx, task.vm_id);
                zombies.push((task.task.id, task.vm_id.clone()));
            }
        }

        while let Some((task_id, vm_id)) = zombies.pop() {
            tracing::debug!(
                "reaping stopped task (id: {}) running on VM {}",
                task_id,
                vm_id
            );

            // For each zombie task:
            // - Stop the VM.
            // - Remove the VM from the `running_vms`.
            // - Remove the task from the `running_tasks`.
            //
            if let Some(idx) = running_vms.iter().position(|x| x.vm_id().eq(vm_id.clone())) {
                let vm_handle = running_vms.swap_remove(idx);
                if let Err(err) = self
                    .program_manager
                    .lock()
                    .await
                    .stop_program(vm_handle)
                    .await
                {
                    tracing::error!("failed to stop VM {}: {}", vm_id, err);
                }
            }

            if let Some(idx) = running_tasks.iter().position(|x| x.task.id == task_id) {
                let _ = running_tasks.swap_remove(idx);
            }
        }

        // Now, reap VMs that don't have running task.
        let mut zombies = vec![];
        for vm_handle in running_vms.iter() {
            if !running_tasks.iter().any(|x| x.vm_id.eq(vm_handle.vm_id())) {
                // XXX: Following is not 100% correct. It checks the runtime
                // from the very beginning of the whole VM's start-up, which
                // is not the same as idle runtime. However, right now each VM
                // is expected to be run only for one task and this check is
                // not visited until there's no running task for the given VM
                // so it's ought to be ok.
                if vm_handle.run_time() > MAX_VM_IDLE_RUN_TIME {
                    tracing::debug!(
                        "VM {} has been running idle for {:#?}; scheduling for termination",
                        vm_handle.vm_id(),
                        vm_handle.run_time()
                    );
                    zombies.push(vm_handle.vm_id());
                }
            }
        }

        while let Some(vm_id) = zombies.pop() {
            tracing::debug!("reaping idle VM {}", vm_id);

            if let Some(idx) = running_vms.iter().position(|x| x.vm_id().eq(vm_id.clone())) {
                let vm_handle = running_vms.swap_remove(idx);
                if let Err(err) = self
                    .program_manager
                    .lock()
                    .await
                    .stop_program(vm_handle)
                    .await
                {
                    tracing::error!("failed to stop VM {}: {}", vm_id, err);
                }
            }
        }
    }
}

#[async_trait]
impl TaskManager for Scheduler {
    async fn get_task(&self, program: Hash, vm_id: Arc<dyn VMId>) -> Option<grpc::Task> {
        tracing::debug!(
            "program {} running in vm_id {} requests for new task",
            program,
            vm_id
        );

        if let Some(task_queue) = self.task_queue.lock().await.get(&program) {
            if let Some((task, scheduled)) = task_queue.front() {
                tracing::debug!(
                    "task {} found for program {} running in vm_id {}",
                    task.id,
                    program,
                    vm_id
                );
                tracing::info!(
                    "task {} started in {}ms",
                    task.id,
                    scheduled.elapsed().as_millis()
                );

                self.running_tasks.lock().await.push(RunningTask {
                    task: task.clone(),
                    vm_id: vm_id.clone(),
                    task_scheduled: *scheduled,
                    task_started: Instant::now(),
                });

                return Some(grpc::Task {
                    id: task.tx.to_string(),
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
        program: Hash,
        vm_id: Arc<dyn VMId>,
        result: grpc::task_result_request::Result,
    ) -> bool {
        dbg!(&result);

        let grpc::task_result_request::Result::Task(result) = result else {
            todo!("task failed; handle it correctly")
        };

        let task_id = &result.id;
        let mut running_tasks = self.running_tasks.lock().await;
        if let Some(idx) = running_tasks
            .iter()
            .position(|e| &e.task.tx.to_string() == task_id)
        {
            let running_task = running_tasks.swap_remove(idx);
            tracing::info!(
                "task {} finished in {}sec",
                task_id,
                running_task.task_started.elapsed().as_secs()
            );

            if let Err(err) = self.database.mark_tx_executed(&running_task.task.tx).await {
                tracing::error!(
                    "failed to update transaction.executed => true - tx.hash: {} error:{err}",
                    &running_task.task.tx
                );
            }

            //move and build output files of the Tx execution
            let executed_files: Vec<(File<Vm>, File<ProofVerif>)> = result
                .files
                .into_iter()
                .map(|file| {
                    let vm_file = File::<Vm>::new(
                        file.path.to_string(),
                        file.checksum[..].into(),
                        running_task.task.tx,
                    );

                    let uuid = Uuid::new_v4();
                    //format file_name to keep the current filename and the uuid.
                    //put uuid at the end so that the uuid is use to save the file.
                    let new_file_name = format!("{}/{}", file.path, uuid);
                    let dest = File::<ProofVerif>::new(
                        file.path,
                        self.http_download_host.clone(),
                        file.checksum[..].into(),
                    );
                    (vm_file, dest)
                })
                .collect();

            //TODO ? -Verify that all expected files has been generated.
            // let executed_files: Vec<(VmFile, TxFile)> =
            //     if let Ok(Some(tx)) = self.database.find_transaction(&running_task.task.tx).await {
            //         if let Payload::Run { workflow } = tx.payload {
            //             workflow
            //                 .steps
            //                 .iter()
            //                 .flat_map(|step| &step.inputs)
            //                 .filter_map(|input| {
            //                     let ProgramData::Output {
            //                         source_program,
            //                         file_name,
            //                     } = input
            //                     else {
            //                         return None;
            //                     };
            //                     let vm_file = VmFile {
            //                         task_id: running_task.task.tx,
            //                         name: file_name.to_string(),
            //                     };

            //                     let uuid = Uuid::new_v4();
            //                     //format file_name to keep the current filename and the uuid.
            //                     //put uuid at the end so that the uuid is use to save the file.
            //                     let new_file_name = format!("{}/{}", file_name, uuid);
            //                     let dest = TxFile {
            //                         //Real url will be calculated during download.
            //                         url: self.http_download_host.clone(),
            //                         name: new_file_name,
            //                         checksum: Hash::default(),
            //                     };
            //                     Some((vm_file, dest))
            //                 })
            //                 .collect()
            //         } else {
            //             vec![]
            //         }
            //     } else {
            //         vec![]
            //     };

            let new_tx_files: Vec<File<ProofVerif>> = executed_files
                .iter()
                .map(|(_, file)| file)
                .cloned()
                .collect();
            let nonce = rand::thread_rng().next_u64();
            let mut tx = match running_task.task.kind {
                TaskKind::Proof => Transaction::new(
                    Payload::Proof {
                        parent: running_task.task.tx,
                        prover: program,
                        proof: result.data,
                        files: new_tx_files,
                    },
                    &self.node_key,
                ),
                TaskKind::Verification => Transaction::new(
                    Payload::Verification {
                        parent: running_task.task.tx,
                        verifier: program,
                        verification: result.data,
                        files: new_tx_files,
                    },
                    &self.node_key,
                ),
                TaskKind::PoW => {
                    todo!("proof of work tasks not implemented yet");
                }
                TaskKind::Nop => {
                    panic!(
                        "impossible to receive result from a task ({}/{}) with task.kind == Nop",
                        running_task.task.id, running_task.task.tx
                    );
                }
            };

            //TODO Add tx execution in the state need some Higher Kind type.
            tx.executed = true;

            tracing::info!("Submit result Tx created:{}", tx.hash.to_string());

            //Move tx file from execution Tx path to new Tx path
            for (source_file, dest_file) in executed_files {
                if let Err(err) =
                    move_vmfile(&source_file, &dest_file, &self.data_directory, tx.hash).await
                {
                    tracing::error!(
                        "failed to move excution file from: {source_file:?} to: {dest_file:?} error: {err}",
                    );
                }
            }

            //send tx to validation process.
            if let Err(err) = self.tx_sender.send_tx(tx).await {
                tracing::error!("failed to send Tx result to validation process: {}", err);
            } else {
                tracing::info!("successfully sent new tx to tx validation from task result");
            }

            tracing::debug!("terminating VM {} running program {}", vm_id, program);

            let mut running_vms = self.running_vms.lock().await;
            let idx = running_vms.iter().position(|e| e.vm_id().eq(vm_id.clone()));
            if idx.is_some() {
                let program_handle = running_vms.remove(idx.unwrap());
                if let Err(err) = self
                    .program_manager
                    .lock()
                    .await
                    .stop_program(program_handle)
                    .await
                {
                    tracing::error!("failed to stop program {}: {}", program, err);
                }
            }
        }

        return false;
    }
}
