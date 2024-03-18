use self::grpc::{TaskResponse, TaskResultRequest};
use super::VMId;
use crate::types::Hash;
use crate::types::Task;
use async_trait::async_trait;
use eyre::Result;
use gevulot_node::types::file::TaskVmFile;
use grpc::vm_service_server::VmService;
use grpc::{TaskRequest, TaskResultResponse};
use std::fmt::Debug;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;
use tonic::{Code, Extensions, Request, Response, Status};

pub mod grpc {
    tonic::include_proto!("vm_service");
}

/// DATA_STREAM_CHUNK_SIZE controls the chunk size for streaming byte
/// transfers, e.g. when transferring the input parameter file to `Program`.
const DATA_STREAM_CHUNK_SIZE: usize = 4096;

/// TaskManager defines interface that `VMServer` uses to pull new tasks for VM's
/// requesting for work and submitting results of tasks.
#[async_trait]
pub trait TaskManager: Send + Sync {
    async fn get_pending_task(&self, tx_hash: Hash) -> Option<Task>;
    async fn get_running_task(&self, tx_hash: Hash) -> Option<Task>;
    async fn submit_result(
        &self,
        tx_hash: Hash,
        program: Hash,
        result: grpc::task_result_request::Result,
    ) -> Result<(), String>;
}

/// ProgramRegistry defines interface that `VMServer` uses to identify which
/// `Program` sent corresponding request.
pub trait ProgramRegistry: Send {
    fn find_by_req(&mut self, extensions: &Extensions) -> Option<(Hash, Hash, Arc<dyn VMId>)>;
}

/// VMServer is the integration point between Gevulot node and individual
/// Nanos VMs. It implements the gRPC interface that program running in VM
/// connects to and communicates with.
pub struct VMServer {
    task_source: Arc<dyn TaskManager>,
    program_registry: Arc<Mutex<dyn ProgramRegistry>>,
    file_data_dir: PathBuf,
}

impl Debug for VMServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "vmserver")
    }
}

impl VMServer {
    pub fn new(
        task_source: Arc<dyn TaskManager>,
        program_registry: Arc<Mutex<dyn ProgramRegistry>>,
        file_data_dir: PathBuf,
    ) -> Self {
        VMServer {
            task_source,
            program_registry,
            file_data_dir,
        }
    }

    pub fn grpc_server(self) -> grpc::vm_service_server::VmServiceServer<VMServer> {
        grpc::vm_service_server::VmServiceServer::new(self)
    }
}

#[tonic::async_trait]
impl VmService for VMServer {
    #[tracing::instrument]
    async fn get_task(
        &self,
        request: Request<TaskRequest>,
    ) -> Result<Response<TaskResponse>, Status> {
        tracing::info!("request for task: {:?}", request);
        let (tx_hash, program_id, vm_id) = self
            .program_registry
            .lock()
            .await
            .find_by_req(request.extensions())
            .ok_or_else(|| {
                Status::new(
                    Code::Unknown,
                    format!("unknown VM address: {:?}", request.remote_addr()),
                )
            })?;

        let reply = match self.task_source.get_pending_task(tx_hash).await {
            Some(task) => grpc::TaskResponse {
                result: Some(grpc::task_response::Result::Task(grpc::Task {
                    tx_hash: task.tx.to_string(),
                    name: task.name.to_string(),
                    args: task.args,
                    files: task
                        .files
                        .into_iter()
                        .map(|x| x.vm_file_path().to_string())
                        .collect(),
                })),
            },
            None => grpc::TaskResponse {
                result: Some(grpc::task_response::Result::Error(
                    grpc::TaskError::Unavailable.into(),
                )),
            },
        };
        Ok(Response::new(reply))
    }

    #[tracing::instrument]
    async fn submit_result(
        &self,
        request: Request<TaskResultRequest>,
    ) -> Result<Response<TaskResultResponse>, Status> {
        let (tx_hash, program_id, vm_id) = match self
            .program_registry
            .lock()
            .await
            .find_by_req(request.extensions())
        {
            Some(instance) => instance,
            None => {
                return Err(Status::new(
                    Code::Unknown,
                    format!("unknown VM address: {:?}", request.remote_addr()),
                ));
            }
        };

        tracing::trace!(
            "VMServer submit_result program:{}, vm_id:{vm_id}",
            program_id.to_string()
        );

        let request = request.into_inner();
        let request_tx_hash: Hash = (&*request.tx_hash).into();

        if tx_hash == request_tx_hash {
            if let Some(result) = request.result {
                if let Err(err) = self
                    .task_source
                    .submit_result(request_tx_hash, program_id, result)
                    .await
                {
                    tracing::error!("Error during submit VM execution result:{err}");
                }
            }
        } else {
            tracing::error!(
                "submit_result different Tx_hash from vm_id:{} and request result:{}",
                tx_hash.to_string(),
                request_tx_hash.to_string()
            );
        }

        // Clean VM `/workspace` data.
        let workspace_path = TaskVmFile::get_workspace_path(&self.file_data_dir, request_tx_hash);
        if let Err(err) = std::fs::remove_dir_all(workspace_path) {
            tracing::warn!(
                "Execution workspace for Tx:{} didn't clean correctly because {err}",
                tx_hash
            );
        }

        let reply = grpc::TaskResultResponse { r#continue: false };
        Ok(Response::new(reply))
    }
}
