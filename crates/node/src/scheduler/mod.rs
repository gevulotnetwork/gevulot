use crate::types::transaction::Created;
use crate::vmm::VMHandle;
use gevulot_node::types::transaction::Validated;
use tokio_stream::StreamExt;
mod program_manager;
pub mod resource_manager;

use self::program_manager::ProgramHandle;
use crate::cli::Config;
use crate::storage::Database;
use crate::txvalidation;
use crate::txvalidation::CallbackSender;
use crate::txvalidation::TxEventSender;
use crate::txvalidation::TxResultSender;
use crate::types::file::{move_vmfile, Output, TaskVmFile, TxFile, VmOutput};
use crate::vmm::qemu::Qemu;
use crate::vmm::vm_server::grpc;
use crate::vmm::vm_server::ShareVMRunningTaskMap;
use crate::vmm::vm_server::VMServer;
use crate::workflow::{WorkflowEngine, WorkflowError};
use crate::{
    mempool::Mempool,
    types::{Hash, Task},
};
use eyre::Result;
use futures::stream::FuturesUnordered;
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
use thiserror::Error;
use tokio::select;
use tokio::sync::mpsc::Sender;
use tokio::{sync::RwLock, time::sleep};
use tonic::transport::Server;

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

pub async fn start_scheduler(
    config: Arc<Config>,
    storage: Arc<Database>,
    mempool: Arc<RwLock<Mempool>>,
    node_key: SecretKey,
    tx_sender: Sender<(Transaction<Received>, Option<CallbackSender>)>,
    exec_receiver: tokio::sync::mpsc::Receiver<Transaction<Validated>>,
) {
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

    let resource_manager = ResourceManager::new(available_mem, num_cpus, num_gpus);

    // TODO(tuommaki): Handle provider from config.
    let qemu_provider = Qemu::new(config.clone());
    let vsock_stream = qemu_provider.vm_server_listener().expect("vsock bind");

    let workflow_engine = Arc::new(WorkflowEngine::new(storage.clone()));
    let download_url_prefix = format!(
        "http://{}:{}",
        config.p2p_listen_addr.ip(),
        config.http_download_port
    );

    let scheduler = Arc::new(Scheduler::new(
        mempool.clone(),
        storage.clone(),
        workflow_engine,
        node_key,
        config.data_directory.clone(),
        config.log_directory.clone(),
        download_url_prefix,
        txvalidation::TxEventSender::<txvalidation::TxResultSender>::build(tx_sender.clone()),
    ));

    //Define submit result channel
    let (submit_result_tx, submit_result_rx) = tokio::sync::mpsc::channel(1000);

    //define task mutex between VM exe and scheduler
    let share_running_task = ShareVMRunningTaskMap::new();
    let vm_server = VMServer::new(
        share_running_task.clone(),
        config.data_directory.clone(),
        submit_result_tx,
    );

    // Start gRPC VSOCK server.
    tokio::spawn(async move {
        Server::builder()
            .add_service(vm_server.grpc_server())
            .serve_with_incoming(vsock_stream)
            .await
    });

    // Run Scheduler in its own task.
    tokio::spawn(async move {
        scheduler
            .run2(
                qemu_provider,
                resource_manager,
                share_running_task,
                mempool,
                submit_result_rx,
                exec_receiver,
            )
            .await
    });
}

#[allow(clippy::enum_variant_names)]
#[derive(Error, Debug)]
pub enum ExecuteTaskError {
    #[error("Task:{0} Execution: Input file copy fail: {1}")]
    InputFileCopyFailError(Hash, u32, String),
    #[error("Task Execution: Allocation fail for: {0}")]
    NotEnoughResources(String),
    #[error("Task Execution: Program:{0} not found")]
    ProgramNotFound(Hash),
    #[error("Task Execution: Error from the database:{0}")]
    DatabaseError(String),
    #[error("Task Execution: Start of the VM fail:{0}")]
    VMStartExecutionFail(String),
    #[error("Task Execution: Start of the VM fail:{0}")]
    VMExecutionError(Hash, u32, String),
    #[error("Task Execution: error during submit result:{0}")]
    SubmitResultError(String),
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
    //    state: Arc<Mutex<SchedulerState>>,
    data_directory: PathBuf,
    log_directory: PathBuf,
    http_download_host: String,
    tx_sender: TxEventSender<TxResultSender>,
}

impl Scheduler {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        mempool: Arc<RwLock<Mempool>>,
        database: Arc<Database>,
        workflow_engine: Arc<WorkflowEngine>,
        node_key: SecretKey,
        data_directory: PathBuf,
        log_directory: PathBuf,
        http_download_host: String,
        tx_sender: TxEventSender<TxResultSender>,
    ) -> Self {
        Self {
            database,
            workflow_engine,
            node_key,

            //            state: Arc::new(Mutex::new(SchedulerState::new(mempool, program_manager))),
            data_directory,
            log_directory,
            http_download_host,
            tx_sender,
        }
    }

    pub async fn run2(
        &self,
        vm_provider: Qemu,
        resource_manager: ResourceManager,
        running_task: ShareVMRunningTaskMap,
        mempool: Arc<RwLock<Mempool>>,
        mut result_receiver: tokio::sync::mpsc::Receiver<(
            Task,
            u32,
            grpc::task_result_request::Result,
        )>,
        mut exec_receiver: tokio::sync::mpsc::Receiver<Transaction<Validated>>,
    ) -> Result<()> {
        let mut start_vm_futures = FuturesUnordered::new();
        let mut torun_task_futures = FuturesUnordered::new();
        let mut result_vm_futures = FuturesUnordered::new();
        let mut task_manager = ProgramManager::new(
            self.database.clone(),
            vm_provider,
            resource_manager,
            running_task,
        );

        let mut running_task: HashMap<Hash, VMHandle> = HashMap::new();

        //Reap zombies every  second.
        let mut reap_zombies_timer = tokio::time::interval(Duration::from_millis(1000));

        loop {
            select! {
                //Everify Zombies  VM.
                _ = reap_zombies_timer.tick() => {
                    //TODO.
                }

                //process task execution result
                Some((task, cid, result)) = result_receiver.recv() => {
                    let result_jh = tokio::spawn({
                        let http_download_host = self.http_download_host.clone();
                        let data_directory = self.data_directory.clone();
                        let node_key = self.node_key.clone();
                        let database = self.database.clone();
                        async move {
                            let tx_hash = task.tx;
                            let res = Scheduler::submit_result(task, cid, result, &http_download_host, &data_directory, node_key, &database).await;

                            //TODO put inn the right place. VM exe error doesn't remove workplace
                            // Clean VM `<tx_hash>/workspace` data.
                            let workspace_path = TaskVmFile::get_workspace_path(&data_directory, tx_hash);
                            let to_remove_path = workspace_path.parent().unwrap(); //unwrap always a parent.
                            if let Err(err) = std::fs::remove_dir_all(to_remove_path) {
                                tracing::warn!(
                                    "Execution workspace:{:?} for Tx:{tx_hash} didn't clean correctly because {err}",
                                    to_remove_path,
                                );
                            }
                            res
                        }});
                    result_vm_futures.push(result_jh);
                }

                //End task execution.
                Some(Ok((task, cid, res)))= result_vm_futures.next() => {
                    //Manage VM
                    let resource_allocation = match running_task.remove(&task.tx) {
                        Some(mut vm_handle) =>  {
                            tracing::info!(
                                "task of Tx {} finished in {}sec",
                                task.tx,
                                vm_handle.start_time.elapsed().as_secs()
                            );
                            let ressource_allocation = Some(vm_handle.qemu_vm_handle.resource_allocation.clone());
                            tokio::task::spawn_blocking(move || {
                                let cid = vm_handle.qemu_vm_handle.cid;
                                if let Err(err) = vm_handle.qemu_vm_handle
                                    .child
                                    .as_mut()
                                    .ok_or(std::io::Error::other(
                                        "No child process defined for this handle",
                                    ))
                                    .and_then(|p| {
                                        p.kill()?;
                                        p.wait()
                                })  {
                                    tracing::error!("Error during VM kill:{err}");
                                }
                            });
                            ressource_allocation
                        }
                        None => {
                            //the start VM task result hasn't been processed.
                            //The VM execute to fast.
                            //reschedule the result.
                            tracing::warn!("Running VM notify too early Tx result: {} cid:{}", task.tx, cid);
                            result_vm_futures.push(tokio::spawn(async move{
                                sleep(Duration::from_millis(100)).await;
                                (task, cid, res)
                            }));
                            continue;
                        },
                    };
                    task_manager.end_task(task.tx, cid, resource_allocation.as_ref()).await;

                    //manage Tx execution result
                    match res {
                        Ok(Some(tx)) => {
                            // Send tx to validation process.
                            if let Err(err) = self.tx_sender.send_tx(tx).await {
                                tracing::error!("failed to send Tx result to validation process: {}", err)
                            };
                        }
                        Ok(None) => (),
                        Err(err) => {
                            tracing::error!("Error during submit VM execution result:{err}");
                        }
                    }
                }

                //get a task from mempool if any.
                Some(tx) = exec_receiver.recv() => {
                    if let Some(task) = self.get_task_from_tx(&tx).await {
                        torun_task_futures.push(tokio::spawn(async move{task}));
                    }
                }
                Some(Ok(task)) = torun_task_futures.next() => {
                    tracing::debug!("task {}/{} scheduled for running", task.id, task.tx);
                    let qemu_vm_handle = match task_manager.prepare_task_execution(&task).await {
                        Ok(handle) => handle,
                        Err(err) => {
                            match err {
                                ExecuteTaskError::NotEnoughResources(_) => {
                                    tracing::info!("{err} reschedule");
                                    let task_jh = tokio::spawn(
                                        async move {
                                            sleep(Duration::from_millis(500)).await;
                                            task
                                        }
                                    );
                                    //reshedule the task.
                                    torun_task_futures.push(task_jh);
                                }
                                _ => {
                                    tracing::error!("{err} Abort Tx:{}", task.tx);
                                    if let Err(err) = self.database.mark_tx_executed(&task.tx).await
                                    {
                                        tracing::error!(
                                            "failed to update transaction.executed => true - tx.hash: {}",
                                            task.tx
                                        );
                                    }
                                }
                            }
                            continue;
                        }
                    };

                    let validate_jh = tokio::spawn({
                        let data_directory = self.data_directory.clone();
                        let log_directory = self.log_directory.clone();
                        async move {
                            ProgramManager::start_task(task, qemu_vm_handle, None, &data_directory, &log_directory).await
                        }});
                    start_vm_futures.push(validate_jh);
                }
                Some(Ok(res)) = start_vm_futures.next() =>  {
                    match res {
                        Ok(vm_handle) => {
                            running_task.insert(vm_handle.qemu_vm_handle.tx_hash, vm_handle);
                        }
                        Err(err) => {
                            tracing::error!("Could start the VM:{err}");
                            match err {
                                ExecuteTaskError::InputFileCopyFailError(tx_hash, cid, _) => {
                                    task_manager.end_task(tx_hash, cid, None).await;
                                },
                                _ => {

                                }
                            }

                        }
                    }
                }
            }
        }
    }

    async fn get_task_from_tx(&self, tx: &Transaction<Validated>) -> Option<Task> {
        match self.workflow_engine.next_task(tx).await {
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
        }
    }
    async fn submit_result(
        task: Task,
        cid: u32,
        result: grpc::task_result_request::Result,
        http_download_host: &str,
        data_directory: &PathBuf,
        node_key: SecretKey,
        database: &Database,
    ) -> (
        Task,
        u32,
        Result<Option<Transaction<Created>>, ExecuteTaskError>,
    ) {
        tracing::debug!(
            "submit_result tx:{} cid:{}  result:{result:#?}",
            task.tx,
            cid
        );

        if let Err(err) = database.mark_tx_executed(&task.tx).await {
            tracing::error!(
                "failed to update transaction.executed => true - tx.hash: {}",
                &task.tx
            );
        }

        match result {
            grpc::task_result_request::Result::Task(result) => {
                // Handle tx execution's result files so that they are available as an input for next task if needed.
                let executed_files: Vec<(TaskVmFile<VmOutput>, TxFile<Output>)> = result
                    .files
                    .into_iter()
                    .map(|file| {
                        let vm_file = TaskVmFile::<VmOutput>::new(file.path.to_string(), task.tx);
                        let dest = TxFile::<Output>::new(
                            file.path,
                            http_download_host.to_string(),
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
                let tx = match task.kind {
                    TaskKind::Proof => Transaction::new(
                        Payload::Proof {
                            parent: task.tx,
                            prover: task.program_id,
                            proof: result.data,
                            files: new_tx_files,
                        },
                        &node_key,
                    ),
                    TaskKind::Verification => Transaction::new(
                        Payload::Verification {
                            parent: task.tx,
                            verifier: task.program_id,
                            verification: result.data,
                            files: new_tx_files,
                        },
                        &node_key,
                    ),
                    TaskKind::PoW => {
                        todo!("proof of work tasks not implemented yet");
                    }
                    TaskKind::Nop => {
                        panic!(
                                "impossible to receive result from a task ({}/{}) with task.kind == Nop",
                                task.id, task.tx
                            );
                    }
                };
                tracing::info!("Submit result Tx created:{}", tx.hash.to_string());

                // Move tx file from execution Tx path to new Tx path
                for (source_file, dest_file) in executed_files {
                    if let Err(err) =
                        move_vmfile(&source_file, &dest_file, data_directory, tx.hash).await
                    {
                        return (task, cid, Err(ExecuteTaskError::SubmitResultError(
                                format!("failed to move excution file from: {source_file:?} to: {dest_file:?} error: {err}")
                            )));
                    }
                }

                tracing::debug!("terminating VM running program {}", task.program_id);
                (task, cid, Ok(Some(tx)))
            }
            grpc::task_result_request::Result::Error(error) => {
                tracing::warn!("Error during Tx:{} execution:{error:?}", task.tx);
                (task, cid, Ok(None))
            }
        }
    }

    //     async fn reap_zombies(&self) {
    //         let mut state = self.state.lock().await;

    //         let mut zombies = vec![];
    //         for task in state.running_tasks.values() {
    //             if let Some(vm_handle) = state.running_vms.get(&task.task.tx) {
    //                 tracing::trace!(
    //                     "inspecting if VM for task id {} is still alive",
    //                     task.task.id
    //                 );
    //                 match vm_handle.is_alive().await {
    //                     Ok(true) => {
    //                         if task.task_started.elapsed() > MAX_VM_RUN_TIME {
    //                             tracing::trace!("VM is still alive but has exceeded MAX_VM_RUN_TIME ({:#?} > {:#?}) - terminating it.", task.task_started.elapsed(), MAX_VM_RUN_TIME);
    //                             zombies.push(task.task.tx);
    //                         } else {
    //                             tracing::trace!(
    //                                 "VM is still alive (run time: {:#?}).",
    //                                 task.task_started.elapsed()
    //                             );
    //                             continue; // All good
    //                         }
    //                     }
    //                     Ok(false) => {
    //                         // VM is not running anymore.
    //                         tracing::warn!(
    //                             "VM {} running for task (id: {}, tx hash: {}) has stopped",
    //                             vm_handle.vm_id(),
    //                             task.task.id,
    //                             task.task.tx
    //                         );
    //                         zombies.push(task.task.tx);
    //                     }
    //                     Err(err) => {
    //                         tracing::error!("failed to query VM state for {} running task (id: {}, tx hash: {}): {}", vm_handle.vm_id(), task.task.id, task.task.tx, err);
    //                         zombies.push(task.task.tx);
    //                     }
    //                 }
    //             } else {
    //                 // VM doesn't exist. Shouldn't happen...
    //                 tracing::error!("found task (id: {}, tx hash: {}) running on VM but not the corresponding running VM", task.task.id, task.task.tx);
    //                 zombies.push(task.task.tx);
    //             }
    //         }

    //         while let Some(task_tx) = zombies.pop() {
    //             tracing::debug!("reaping stopped task (tx: {}) running on VM", task_tx,);

    //             // For each zombie task:
    //             // - Stop the VM.
    //             // - Remove the VM from the `running_vms`.
    //             // - Remove the task from the `running_tasks`.
    //             //
    //             if let Some(vm_handle) = state.running_vms.remove(&task_tx) {
    //                 if let Err(err) = state.program_manager.stop_program(vm_handle).await {
    //                     tracing::error!("failed to stop VMvm_handle {}", err);
    //                 }
    //             }

    //             //TODO cancel associated Tx
    //             state.running_tasks.remove(&task_tx);
    //         }

    //         // Now, reap VMs that don't have running task.
    //         let mut zombies = vec![];
    //         for (tx_hash, vm_handle) in state.running_vms.iter() {
    //             if !state.running_tasks.contains_key(tx_hash) {
    //                 // XXX: Following is not 100% correct. It checks the runtime
    //                 // from the very beginning of the whole VM's start-up, which
    //                 // is not the same as idle runtime. However, right now each VM
    //                 // is expected to be run only for one task and this check is
    //                 // not visited until there's no running task for the given VM
    //                 // so it's ought to be ok.
    //                 if vm_handle.run_time() > MAX_VM_IDLE_RUN_TIME {
    //                     tracing::debug!(
    //                         "VM {} has been running idle for {:#?}; scheduling for termination",
    //                         vm_handle.vm_id(),
    //                         vm_handle.run_time()
    //                     );
    //                     zombies.push(*tx_hash);
    //                 }
    //             }
    //         }

    //         while let Some(tx_hash) = zombies.pop() {
    //             tracing::debug!("reaping idle VM for tx {}", tx_hash);

    //             if let Some(idx) = state.running_vms.get(&tx_hash) {
    //                 if let Some(vm_handle) = state.running_vms.remove(&tx_hash) {
    //                     if let Err(err) = state.program_manager.stop_program(vm_handle).await {
    //                         tracing::error!("failed to stop VM  for Tx {}: {}", tx_hash, err);
    //                     }
    //                 }
    //             }
    //         }
    //     }
}

// #[async_trait]
// impl TaskManager for Scheduler {
// async fn get_running_task(&self, tx_hash: Hash) -> Option<Task> {
//     let state = self.state.lock().await;
//     state.running_tasks.get(&tx_hash).map(|t| t.task.clone())
// }

// async fn get_pending_task(&self, tx_hash: Hash) -> Option<Task> {
//     tracing::debug!("tx {} running requests for new task", tx_hash,);

//     // Ensure that the VM requesting for a task is not already executing one!
//     let mut state = self.state.lock().await;
//     if let Some(running_task) = state.running_tasks.get(&tx_hash) {
//         // A VM that is already running a task, requests for a new one.
//         // Mark the existing task as executed and stop the VM.
//         tracing::info!(
//             "task {} has been running {}sec but found to be orphaned. marking it as executed.",
//             running_task.task.id,
//             running_task.task_started.elapsed().as_secs()
//         );

//         tracing::debug!("terminating VM running tx {}", tx_hash);

//         if let Some(program_handle) = state.running_vms.remove(&tx_hash) {
//             if let Err(err) = state.program_manager.stop_program(program_handle).await {
//                 tracing::error!("failed to stop program {}: {}", tx_hash, err);
//             }
//         }

//         return None;
//     }

//     if let Some(task_queue) = state.task_queue.get_mut(&tx_hash) {
//         if let Some((task, scheduled)) = task_queue.pop_front() {
//             tracing::debug!("task {} found for tx {} running", task.id, tx_hash,);
//             tracing::info!(
//                 "task {} started in {}ms",
//                 task.id,
//                 scheduled.elapsed().as_millis()
//             );

//             state.running_tasks.insert(
//                 task.tx,
//                 RunningTask {
//                     task: task.clone(),
//                     task_scheduled: scheduled,
//                     task_started: Instant::now(),
//                 },
//             );

//             return Some(task.clone());
//         }
//     }

//     None
// }

//}
