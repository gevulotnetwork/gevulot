use crate::scheduler::ExecuteTaskError;
use crate::types::program::ResourceRequest;
use eyre::Result;
use thiserror::Error;

#[derive(Debug, Clone)]
pub struct ResourceAllocation {
    //pub(self) resource_manager: Arc<Mutex<ResourceManager>>,
    pub mem: u64,
    pub cpus: u64,
    pub gpus: u64,
}

#[allow(clippy::enum_variant_names)]
#[derive(Error, Debug)]
pub enum ResourceError {
    #[error("not enough resources: {0}")]
    NotEnoughResources(String),
}

#[derive(Debug)]
pub struct ResourceManager {
    available_mem: u64,
    available_cpus: u64,
    available_gpus: u64,
}

impl ResourceManager {
    pub fn new(total_mem: u64, total_cpus: u64, total_gpus: u64) -> Self {
        ResourceManager {
            available_mem: total_mem,
            available_cpus: total_cpus,
            available_gpus: total_gpus,
        }
    }

    pub fn try_allocate(
        &mut self,
        request: &ResourceRequest,
    ) -> Result<ResourceAllocation, ExecuteTaskError> {
        //        let rm = resource_manager.clone();
        //        let mut rm = rm.lock().expect("acquire resource manager instance lock");

        if self.available_mem < request.mem {
            return Err(ExecuteTaskError::NotEnoughResources("memory".to_string()));
        }

        if self.available_cpus < request.cpus {
            return Err(ExecuteTaskError::NotEnoughResources("cpus".to_string()));
        }

        if self.available_gpus < request.gpus {
            return Err(ExecuteTaskError::NotEnoughResources("gpus".to_string()));
        }

        self.available_mem -= request.mem;
        self.available_cpus -= request.cpus;
        self.available_gpus -= request.gpus;

        Ok(ResourceAllocation {
            //resource_manager: resource_manager.clone(),
            mem: request.mem,
            cpus: request.cpus,
            gpus: request.gpus,
        })
    }

    pub fn free(&mut self, allocation: &ResourceAllocation) {
        self.available_mem += allocation.mem;
        self.available_cpus += allocation.cpus;
        self.available_gpus += allocation.gpus;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_try_allocate_succeeds() {
        let rm = Arc::new(Mutex::new(ResourceManager::new(2048, 4, 0)));

        let req = &ResourceRequest {
            mem: 1024,
            cpus: 1,
            gpus: 0,
        };

        ResourceManager::try_allocate(rm.clone(), req).unwrap();
        ResourceManager::try_allocate(rm.clone(), req).unwrap();
    }

    #[test]
    fn test_free_succeeds() {
        let rm = Arc::new(Mutex::new(ResourceManager::new(2048, 4, 0)));

        let req = &ResourceRequest {
            mem: 2048,
            cpus: 4,
            gpus: 0,
        };

        // Allocate all available resources.
        let ra = ResourceManager::try_allocate(rm.clone(), req).unwrap();

        // Assert that we are out of resources.
        let ra2 = ResourceManager::try_allocate(rm.clone(), req);
        assert!(ra2.is_err());

        drop(ra);

        // Allocate again all available resources.
        ResourceManager::try_allocate(rm.clone(), req).unwrap();
    }

    #[test]
    fn test_try_allocate_fails_on_mem() {
        let rm = Arc::new(Mutex::new(ResourceManager::new(2048, 4, 0)));
        let req = &ResourceRequest {
            mem: 4096,
            cpus: 2,
            gpus: 0,
        };

        let ra = ResourceManager::try_allocate(rm, req);
        assert!(ra.is_err());
    }

    #[test]
    fn test_try_allocate_fails_on_cpus() {
        let rm = Arc::new(Mutex::new(ResourceManager::new(2048, 4, 0)));
        let req = &ResourceRequest {
            mem: 1024,
            cpus: 8,
            gpus: 0,
        };

        let ra = ResourceManager::try_allocate(rm, req);
        assert!(ra.is_err());
    }

    #[test]
    fn test_try_allocate_fails_on_gpus() {
        let rm = Arc::new(Mutex::new(ResourceManager::new(2048, 4, 0)));
        let req = &ResourceRequest {
            mem: 1024,
            cpus: 1,
            gpus: 1,
        };

        let ra = ResourceManager::try_allocate(rm, req);
        assert!(ra.is_err());
    }
}
