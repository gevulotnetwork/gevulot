mod program_manager;
mod resource_manager;

use crate::cli::Config;
use crate::storage::Database;
use crate::txvalidation;
use crate::txvalidation::CallbackSender;
use crate::txvalidation::TxEventSender;
use crate::txvalidation::TxResultSender;
use crate::types::file::{move_vmfile, Output, TaskVmFile, TxFile, VmOutput};
use crate::types::TaskState;
use crate::vmm::qemu::Qemu;
use crate::vmm::vm_server::grpc;
use crate::vmm::vm_server::TaskManager;
use crate::vmm::vm_server::VMServer;
use crate::workflow::{WorkflowEngine, WorkflowError};
use crate::{
    mempool::Mempool,
    types::{Hash, Task},
};
use async_trait::async_trait;
use eyre::Result;
use gevulot_node::types::transaction::Payload;
use gevulot_node::types::transaction::Received;
use gevulot_node::types::{TaskKind, Transaction};
use libsecp256k1::SecretKey;
pub use program_manager::ProgramManager;
use rand::RngCore;
pub use resource_manager::ResourceManager;
use std::path::PathBuf;
use std::time::Instant;
use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
    time::Duration,
};
use systemstat::ByteSize;
use systemstat::Platform;
use systemstat::System;
use tokio::sync::mpsc::UnboundedSender;
use tokio::{
    sync::{Mutex, RwLock},
    time::sleep,
};
use tonic::transport::Server;

use self::program_manager::{ProgramError, ProgramHandle};
use self::resource_manager::ResourceError;

// If VM doesn't have running task within `MAX_VM_IDLE_RUN_TIME`, it will be terminated.
const MAX_VM_IDLE_RUN_TIME: Duration = Duration::from_secs(10);
// MAX_VM_RUN_TIME is the maximum time a VM can run no matter what.
// The proof must be generated within this time limit.
const MAX_VM_RUN_TIME: Duration = Duration::from_secs(1800);

struct RunningTask {
    task: Task,
    task_scheduled: Instant,
    task_started: Instant,
}

struct SchedulerState {
    mempool: Arc<RwLock<Mempool>>,
    program_manager: ProgramManager,

    pending_programs: VecDeque<(Hash, Hash)>,
    running_tasks: HashMap<Hash, RunningTask>,
    running_vms: HashMap<Hash, ProgramHandle>,
    task_queue: HashMap<Hash, VecDeque<(Task, Instant)>>,
}

impl SchedulerState {
    fn new(mempool: Arc<RwLock<Mempool>>, program_manager: ProgramManager) -> Self {
        Self {
            mempool,
            program_manager,
            pending_programs: VecDeque::new(),
            running_tasks: HashMap::new(),
            running_vms: HashMap::new(),
            task_queue: HashMap::new(),
        }
    }
}

// TODO: I believe `Scheduler` should be rather named `Node` at some point. It
// has started to become as a kind of central place for node logic
// coordination.
//
// It could use some re-structuring in order to not be so convoluted, but
// otherwise it seems to be okayish.
pub struct Scheduler {
    database: Arc<Database>,
    workflow_engine: Arc<WorkflowEngine>,
    node_key: SecretKey,

    state: Arc<Mutex<SchedulerState>>,
    data_directory: PathBuf,
    http_download_host: String,
    tx_sender: TxEventSender<TxResultSender>,
}

pub async fn start_scheduler(
    config: Arc<Config>,
    storage: Arc<Database>,
    mempool: Arc<RwLock<Mempool>>,
    node_key: SecretKey,
    tx_sender: UnboundedSender<(Transaction<Received>, Option<CallbackSender>)>,
) -> Arc<Scheduler> {
    let sys = System::new();
    let num_gpus = if config.gpu_devices.is_some() { 1 } else { 0 };
    let num_cpus = match config.num_cpus {
        Some(cpus) => cpus,
        None => num_cpus::get() as u64,
    };
    let available_mem = match config.mem_gb {
        Some(mem_gb) => mem_gb * 1024 * 1024 * 1024,
        None => {
            let mem = sys
                .memory()
                .expect("failed to lookup available system memory");
            mem.total.as_u64()
        }
    };

    tracing::info!(
        "node configured with {} CPUs, {} MEM and {} GPUs",
        num_cpus,
        ByteSize(available_mem).to_string_as(true),
        num_gpus
    );

    let resource_manager = Arc::new(std::sync::Mutex::new(ResourceManager::new(
        available_mem,
        num_cpus,
        num_gpus,
    )));

    // TODO(tuommaki): Handle provider from config.
    let qemu_provider = Qemu::new(config.clone());
    let vsock_stream = qemu_provider.vm_server_listener().expect("vsock bind");

    let provider = Arc::new(Mutex::new(qemu_provider));
    let program_manager =
        ProgramManager::new(storage.clone(), provider.clone(), resource_manager.clone());

    let workflow_engine = Arc::new(WorkflowEngine::new(storage.clone()));
    let download_url_prefix = format!(
        "http://{}:{}",
        config.p2p_listen_addr.ip(),
        config.http_download_port
    );

    let scheduler = Arc::new(Scheduler::new(
        mempool.clone(),
        storage.clone(),
        program_manager,
        workflow_engine,
        node_key,
        config.data_directory.clone(),
        download_url_prefix,
        txvalidation::TxEventSender::<txvalidation::TxResultSender>::build(tx_sender.clone()),
    ));

    let vm_server = VMServer::new(scheduler.clone(), provider, config.data_directory.clone());

    // Start gRPC VSOCK server.
    tokio::spawn(async move {
        Server::builder()
            .add_service(vm_server.grpc_server())
            .serve_with_incoming(vsock_stream)
            .await
    });
    scheduler
}

impl Scheduler {
    #[allow(clippy::too_many_arguments)]
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
            database,
            workflow_engine,
            node_key,

            state: Arc::new(Mutex::new(SchedulerState::new(mempool, program_manager))),
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
                let mut state = self.state.lock().await;
                while let Some((tx_hash, program_id)) = state.pending_programs.pop_front() {
                    match state
                        .program_manager
                        .start_program(tx_hash, program_id, None)
                        .await
                    {
                        Ok(p) => {
                            state.running_vms.insert(tx_hash, p);
                        }
                        Err(e) if e.is::<ResourceError>() => {
                            let err = e.downcast_ref::<ResourceError>().unwrap();
                            tracing::info!("resources unavailable: {}", err);
                            sleep(Duration::from_millis(500)).await;

                            // Return the popped program_id back to pending queue.
                            state.pending_programs.push_front((tx_hash, program_id));
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

            // Copy Task file to VM workspace
            // Validate that everything is ok before starting the task.
            for file in task.files.iter() {
                if let Err(err) = file
                    .copy_file_for_vm_exe(&self.data_directory, task.tx)
                    .await
                {
                    tracing::error!(
                        "An error occurs during Tx:{} task file copy for VM execution {err}",
                        task.tx
                    );
                    if let Err(err) = self.database.mark_tx_executed(&task.tx).await {
                        tracing::error!(
                            "failed to update transaction.executed => true - tx.hash: {}",
                            task.tx
                        );
                    }
                    continue;
                }
            }

            // Push the task into program's work queue.
            let mut state = self.state.lock().await;
            if let Some(program_task_queue) = state.task_queue.get_mut(&task.tx) {
                program_task_queue.push_back((task.clone(), Instant::now()));
            } else {
                let mut queue = VecDeque::new();
                queue.push_back((task.clone(), Instant::now()));
                state.task_queue.insert(task.tx, queue);
            }

            // Start the program.
            match state
                .program_manager
                .start_program(task.tx, task.program_id, None)
                .await
            {
                Ok(p) => {
                    state.running_vms.insert(task.tx, p);
                }
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
                        state.task_queue.remove(&task.tx);
                    }
                }
            }
        }
    }

    async fn pick_task(&self) -> Option<Task> {
        let state = self.state.lock().await;

        // Acquire write lock.
        let mut mempool = state.mempool.write().await;

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
        self.state
            .lock()
            .await
            .pending_programs
            .push_back((task.tx, task.program_id));
        Ok(())
    }

    async fn reap_zombies(&self) {
        let mut state = self.state.lock().await;

        let mut zombies = vec![];
        for task in state.running_tasks.values() {
            if let Some(vm_handle) = state.running_vms.get(&task.task.tx) {
                tracing::trace!(
                    "inspecting if VM for task id {} is still alive",
                    task.task.id
                );
                match vm_handle.is_alive().await {
                    Ok(true) => {
                        if task.task_started.elapsed() > MAX_VM_RUN_TIME {
                            tracing::trace!("VM is still alive but has exceeded MAX_VM_RUN_TIME ({:#?} > {:#?}) - terminating it.", task.task_started.elapsed(), MAX_VM_RUN_TIME);
                            zombies.push(task.task.tx);
                        } else {
                            tracing::trace!(
                                "VM is still alive (run time: {:#?}).",
                                task.task_started.elapsed()
                            );
                            continue; // All good
                        }
                    }
                    Ok(false) => {
                        // VM is not running anymore.
                        tracing::warn!(
                            "VM {} running for task (id: {}, tx hash: {}) has stopped",
                            vm_handle.vm_id(),
                            task.task.id,
                            task.task.tx
                        );
                        zombies.push(task.task.tx);
                    }
                    Err(err) => {
                        tracing::error!("failed to query VM state for {} running task (id: {}, tx hash: {}): {}", vm_handle.vm_id(), task.task.id, task.task.tx, err);
                        zombies.push(task.task.tx);
                    }
                }
            } else {
                // VM doesn't exist. Shouldn't happen...
                tracing::error!("found task (id: {}, tx hash: {}) running on VM but not the corresponding running VM", task.task.id, task.task.tx);
                zombies.push(task.task.tx);
            }
        }

        while let Some(task_tx) = zombies.pop() {
            tracing::debug!("reaping stopped task (tx: {}) running on VM", task_tx,);

            // For each zombie task:
            // - Stop the VM.
            // - Remove the VM from the `running_vms`.
            // - Remove the task from the `running_tasks`.
            //
            if let Some(vm_handle) = state.running_vms.remove(&task_tx) {
                if let Err(err) = state.program_manager.stop_program(vm_handle).await {
                    tracing::error!("failed to stop VMvm_handle {}", err);
                }
            }

            //TODO cancel associated Tx
            state.running_tasks.remove(&task_tx);
        }

        // Now, reap VMs that don't have running task.
        let mut zombies = vec![];
        for (tx_hash, vm_handle) in state.running_vms.iter() {
            if !state.running_tasks.contains_key(tx_hash) {
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
                    zombies.push(*tx_hash);
                }
            }
        }

        while let Some(tx_hash) = zombies.pop() {
            tracing::debug!("reaping idle VM for tx {}", tx_hash);

            if let Some(idx) = state.running_vms.get(&tx_hash) {
                if let Some(vm_handle) = state.running_vms.remove(&tx_hash) {
                    if let Err(err) = state.program_manager.stop_program(vm_handle).await {
                        tracing::error!("failed to stop VM  for Tx {}: {}", tx_hash, err);
                    }
                }
            }
        }
    }
}

#[async_trait]
impl TaskManager for Scheduler {
    async fn get_running_task(&self, tx_hash: Hash) -> Option<Task> {
        let state = self.state.lock().await;
        state.running_tasks.get(&tx_hash).map(|t| t.task.clone())
    }

    async fn get_pending_task(&self, tx_hash: Hash) -> Option<Task> {
        tracing::debug!("tx {} running requests for new task", tx_hash,);

        // Ensure that the VM requesting for a task is not already executing one!
        let mut state = self.state.lock().await;
        if let Some(running_task) = state.running_tasks.get(&tx_hash) {
            // A VM that is already running a task, requests for a new one.
            // Mark the existing task as executed and stop the VM.
            tracing::info!(
                "task {} has been running {}sec but found to be orphaned. marking it as executed.",
                running_task.task.id,
                running_task.task_started.elapsed().as_secs()
            );

            tracing::debug!("terminating VM running tx {}", tx_hash);

            if let Some(program_handle) = state.running_vms.remove(&tx_hash) {
                if let Err(err) = state.program_manager.stop_program(program_handle).await {
                    tracing::error!("failed to stop program {}: {}", tx_hash, err);
                }
            }

            return None;
        }

        if let Some(task_queue) = state.task_queue.get_mut(&tx_hash) {
            if let Some((task, scheduled)) = task_queue.pop_front() {
                tracing::debug!("task {} found for tx {} running", task.id, tx_hash,);
                tracing::info!(
                    "task {} started in {}ms",
                    task.id,
                    scheduled.elapsed().as_millis()
                );

                state.running_tasks.insert(
                    task.tx,
                    RunningTask {
                        task: task.clone(),
                        task_scheduled: scheduled,
                        task_started: Instant::now(),
                    },
                );

                return Some(task.clone());
            }
        }

        None
    }

    async fn submit_result(
        &self,
        tx_hash: Hash,
        program: Hash,
        result: grpc::task_result_request::Result,
    ) -> Result<(), String> {
        tracing::debug!("submit_result tx:{tx_hash}  result:{result:#?}");

        let mut state = self.state.lock().await;
        if let Some(running_task) = state.running_tasks.remove(&tx_hash) {
            tracing::info!(
                "task of Tx {} finished in {}sec",
                running_task.task.tx,
                running_task.task_started.elapsed().as_secs()
            );

            if let Err(err) = self.database.mark_tx_executed(&running_task.task.tx).await {
                tracing::error!(
                    "failed to update transaction.executed => true - tx.hash: {}",
                    &running_task.task.tx
                );
            }

            match result {
                grpc::task_result_request::Result::Task(result) => {
                    // Handle tx execution's result files so that they are available as an input for next task if needed.
                    let executed_files: Vec<(TaskVmFile<VmOutput>, TxFile<Output>)> = result
                        .files
                        .into_iter()
                        .map(|file| {
                            let vm_file =
                                TaskVmFile::<VmOutput>::new(file.path.to_string(), tx_hash);
                            let dest = TxFile::<Output>::new(
                                file.path,
                                self.http_download_host.clone(),
                                file.checksum[..].into(),
                            );
                            (vm_file, dest)
                        })
                        .collect();

                    //TODO -Verify that all expected files has been generated.

                    let new_tx_files: Vec<TxFile<Output>> = executed_files
                        .iter()
                        .map(|(_, file)| file)
                        .cloned()
                        .collect();
                    let nonce = rand::thread_rng().next_u64();
                    let tx = match running_task.task.kind {
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
                    tracing::info!("Submit result Tx created:{}", tx.hash.to_string());

                    // Move tx file from execution Tx path to new Tx path
                    for (source_file, dest_file) in executed_files {
                        move_vmfile(&source_file, &dest_file, &self.data_directory, tx.hash)
                            .await.map_err(|err| format!("failed to move excution file from: {source_file:?} to: {dest_file:?} error: {err}"))?;
                    }

                    // Send tx to validation process.
                    self.tx_sender.send_tx(tx).await.map_err(|err| {
                        format!("failed to send Tx result to validation process: {}", err)
                    })?;
                }
                grpc::task_result_request::Result::Error(error) => {
                    tracing::warn!("Error during Tx:{tx_hash} execution:{error:?}");
                }
            }

            tracing::debug!("terminating VM running program {}", program);

            match state.running_vms.remove(&tx_hash) {
                Some(program_handle) => {
                    state
                        .program_manager
                        .stop_program(program_handle)
                        .await
                        .map_err(|err| format!("failed to stop program {}: {}", program, err))?;
                }
                None => tracing::warn!("Running VM not found for a Tx result: {tx_hash}"),
            }
            Ok(())
        } else {
            Err(format!("submit_result task:{tx_hash} not found."))
        }
    }
}
