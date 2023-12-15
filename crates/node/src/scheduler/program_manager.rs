use eyre::Result;
use std::sync::{Arc, Mutex};
use thiserror::Error;
use tokio::sync::Mutex as TMutex;

use crate::scheduler::resource_manager::{ResourceAllocation, ResourceManager};
use crate::storage::Database;
use crate::types::program::ResourceRequest;
use crate::types::Hash;
use crate::vmm::{Provider, VMHandle, VMId};

#[allow(clippy::enum_variant_names)]
#[derive(Error, Debug)]
pub enum ProgramError {
    #[error("program not found: {0}")]
    ProgramNotFound(String),
}

pub struct ProgramHandle {
    resource_allocation: ResourceAllocation,
    vm_handle: VMHandle,
}

impl ProgramHandle {
    pub fn vm_id(&self) -> Arc<dyn VMId> {
        self.vm_handle.vm_id()
    }
}

pub struct ProgramManager {
    storage: Arc<Database>,
    resource_manager: Arc<Mutex<ResourceManager>>,
    vm_provider: Arc<TMutex<dyn Provider>>,
}

impl ProgramManager {
    pub fn new(
        storage: Arc<Database>,
        vm_provider: Arc<TMutex<dyn Provider>>,
        resource_manager: Arc<Mutex<ResourceManager>>,
    ) -> Self {
        Self {
            storage,
            resource_manager,
            vm_provider,
        }
    }

    pub async fn start_program(
        &mut self,
        id: Hash,
        limits: Option<ResourceRequest>,
    ) -> Result<ProgramHandle> {
        let program = match self.storage.find_program(&id).await? {
            Some(program) => program,
            None => return Err(ProgramError::ProgramNotFound(id.to_string()).into()),
        };

        let req = limits.unwrap_or(program.limits.clone());
        let resource_allocation =
            ResourceManager::try_allocate(self.resource_manager.clone(), &req)?;
        let vm_handle = self.vm_provider.lock().await.start_vm(program, req).await?;

        Ok(ProgramHandle {
            resource_allocation,
            vm_handle,
        })
    }

    pub async fn stop_program(&mut self, prg_handle: ProgramHandle) -> Result<()> {
        self.vm_provider.lock().await.stop_vm(prg_handle.vm_handle)
    }
}
