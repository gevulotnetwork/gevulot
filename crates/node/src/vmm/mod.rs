use gevulot_node::types::Hash;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;
use std::{any::Any, time::Duration};

use crate::types::{program::ResourceRequest, Program};
use async_trait::async_trait;
use eyre::Result;

pub mod qemu;
pub mod vm_server;

pub trait VMId: Send + Sync + std::fmt::Display {
    fn as_any(&self) -> &dyn Any;
    fn eq(&self, x: Arc<dyn VMId>) -> bool;
}

#[async_trait::async_trait]
pub trait VMClient: Send + Sync {
    async fn is_alive(&self) -> Result<bool>;
}

pub struct VMHandle {
    start_time: Instant,
    vm_id: Arc<dyn VMId>,
    vm_client: Arc<dyn VMClient>,
}

impl VMHandle {
    pub fn vm_id(&self) -> Arc<dyn VMId> {
        self.vm_id.clone()
    }

    pub async fn is_alive(&self) -> Result<bool> {
        self.vm_client.is_alive().await
    }

    pub fn run_time(&self) -> Duration {
        self.start_time.elapsed()
    }
}

#[async_trait]
pub trait Provider: Send + Sync {
    async fn start_vm(
        &mut self,
        tx_hash: Hash,
        program: Program,
        req: ResourceRequest,
    ) -> Result<VMHandle>;
    async fn stop_vm(&mut self, vm: VMHandle) -> Result<()>;

    fn prepare_image(&mut self, program: Program, image: &Path) -> Result<()>;
}
