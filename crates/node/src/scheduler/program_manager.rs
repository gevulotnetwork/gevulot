use crate::scheduler::resource_manager::{ResourceAllocation, ResourceManager};
use crate::scheduler::ExecuteTaskError;
use crate::storage::Database;
use crate::types::program::ResourceRequest;
use crate::types::Hash;
use crate::vmm::qemu::Qemu;
use crate::vmm::vm_server::ShareVMRunningTaskMap;
use crate::vmm::QEMUVMHandle;
use crate::vmm::VMHandle;
use eyre::Result;
use gevulot_node::types::Task;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;

#[allow(clippy::enum_variant_names)]
#[derive(Error, Debug)]
pub enum ProgramError {
    #[error("program not found: {0}")]
    ProgramNotFound(String),
}

pub struct ProgramHandle {
    resource_allocation: ResourceAllocation,
    vm_handle: VMHandle,
    task: Task,
}

impl ProgramHandle {
    // pub fn vm_id(&self) -> Arc<dyn VMId> {
    //     self.vm_handle.vm_id()
    // }

    pub async fn is_alive(&self) -> Result<bool> {
        self.vm_handle.is_alive().await
    }

    pub fn run_time(&self) -> Duration {
        self.vm_handle.run_time()
    }
}

pub struct ProgramManager {
    storage: Arc<Database>,
    vm_provider: Qemu,
    resource_manager: ResourceManager,
    running_task: ShareVMRunningTaskMap,
}

impl ProgramManager {
    pub fn new(
        storage: Arc<Database>,
        vm_provider: Qemu,
        resource_manager: ResourceManager,
        running_task: ShareVMRunningTaskMap,
    ) -> Self {
        Self {
            storage,
            resource_manager,
            vm_provider,
            running_task,
        }
    }

    pub async fn prepare_task_execution(
        &mut self,
        task: &Task,
    ) -> Result<QEMUVMHandle, ExecuteTaskError> {
        let ressource_request: Option<ResourceRequest> = Default::default();
        let program = self
            .storage
            .find_program(&task.program_id)
            .await
            .map_err(|err| ExecuteTaskError::DatabaseError(err.to_string()))?
            .ok_or(ExecuteTaskError::ProgramNotFound(task.program_id))?;

        let ressource_request = ressource_request.or(program.limits).unwrap_or_default();
        let resource_allocation = self.resource_manager.try_allocate(&ressource_request)?;

        let cid = self.vm_provider.allocate_cid();

        let qemu_vm_handle = QEMUVMHandle {
            child: None,
            cid,
            tx_hash: task.tx,
            resource_allocation,
            program,
        };

        // Must VM must be registered before start, because when the VM starts,
        // the program in it starts immediately and queries for task, which
        // requires the VM to be registered for identification.
        self.running_task.insert_task(cid, task.clone()).await;
        Ok(qemu_vm_handle)
    }

    pub async fn start_task(
        task: Task,
        mut qemu_vm_handle: QEMUVMHandle,
        gpu_devices: Option<String>,
        data_directory: &PathBuf, //&self.config.data_directory
        log_directory: &PathBuf,
    ) -> Result<VMHandle, ExecuteTaskError> {
        // Copy Task file to VM workspace
        // Validate that everything is ok before starting the task.
        for file in &task.files {
            file.copy_file_for_vm_exe(data_directory, task.tx)
                .await
                .map_err(|err| {
                    ExecuteTaskError::InputFileCopyFailError(
                        task.tx,
                        qemu_vm_handle.cid,
                        format!("{} : {err}", file.vm_file_path()),
                    )
                })?;
        }

        //start the VM
        let (vm_process, vm_client, start_time) = Qemu::start_vm(
            qemu_vm_handle.cid,
            &qemu_vm_handle.resource_allocation,
            gpu_devices,
            data_directory,
            log_directory,
            task.tx,
            &qemu_vm_handle.program,
        )
        .await?;

        qemu_vm_handle.child = Some(vm_process);
        Ok(VMHandle {
            qemu_vm_handle,
            start_time,
            vm_client: Arc::new(vm_client),
        })
    }

    pub async fn end_task(
        &mut self,
        tx_hash: Hash,
        cid: u32,
        resource_allocation: Option<&ResourceAllocation>,
    ) {
        if let Some(resource_allocation) = resource_allocation {
            self.resource_manager.free(resource_allocation);
        }
        self.vm_provider.release_cid(cid);
    }

    pub async fn remove_zombie_task_with_cid(&self, cid: u32) -> Option<Task> {
        self.running_task.remove_task_with_cid(cid).await
    }
}
