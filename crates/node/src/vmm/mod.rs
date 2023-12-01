use std::any::Any;
use std::path::Path;
use std::sync::Arc;

use crate::types::Program;
use async_trait::async_trait;
use eyre::Result;
use serde::{Deserialize, Serialize};

pub mod qemu;
pub mod vm_server;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ResourceRequest {
    pub mem: u64,
    pub cpus: u64,
    pub gpus: u64,
}

impl Default for ResourceRequest {
    fn default() -> Self {
        Self {
            mem: 8192,
            cpus: 8,
            gpus: 0,
        }
    }
}

pub trait VMId: Send + Sync {
    fn as_any(&self) -> &dyn Any;
    fn eq(&self, x: Arc<dyn VMId>) -> bool;
}

pub struct VMHandle {
    vm_id: Arc<dyn VMId>,
}

impl VMHandle {
    pub fn vm_id(&self) -> Arc<dyn VMId> {
        self.vm_id.clone()
    }
}

#[async_trait]
pub trait Provider: Send + Sync {
    async fn start_vm(&mut self, program: Program, req: ResourceRequest) -> Result<VMHandle>;
    fn stop_vm(&mut self, vm: VMHandle) -> Result<()>;

    fn prepare_image(&mut self, program: Program, image: &Path) -> Result<()>;
}
