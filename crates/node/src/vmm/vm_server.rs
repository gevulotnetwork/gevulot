use self::grpc::{TaskResponse, TaskResultRequest};
use super::VMId;
use crate::types::Hash;
use crate::types::Task;
use async_trait::async_trait;
use eyre::Result;
use grpc::vm_service_server::VmService;
use grpc::{TaskRequest, TaskResultResponse};
use std::collections::HashMap;
use std::fmt::Debug;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_vsock::VsockConnectInfo;
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

#[derive(Clone, Debug)]
pub struct ShareVMRunningTaskMap {
    //Map of running task: cid -> (Tx_Hash, Program Hash)
    task_list: Arc<Mutex<HashMap<u32, Task>>>,
}

impl ShareVMRunningTaskMap {
    pub fn new() -> Self {
        ShareVMRunningTaskMap {
            //Map of running task: cid -> (Tx_Hash, Program Hash)
            task_list: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn insert_task(&self, cid: u32, task: Task) {
        self.task_list.lock().await.insert(cid, task.clone());
    }

    // pub async fn remove_task(&self, cid: u32) -> Option<Task> {
    //     self.task_list.lock().await.remove(&cid)
    // }

    pub async fn remove_task_with_extension(&self, extensions: &Extensions) -> Option<(u32, Task)> {
        let conn_info = extensions.get::<VsockConnectInfo>().unwrap();
        match conn_info.peer_addr() {
            Some(addr) => self
                .task_list
                .lock()
                .await
                .remove(&addr.cid())
                .map(|task| (*&addr.cid(), task)),
            None => {
                tracing::error!("remove task fail no task found for extention",);
                None
            }
        }
    }

    pub async fn len(&self) -> usize {
        self.task_list.lock().await.len()
    }

    async fn get_running_task(&self, extensions: &Extensions) -> Option<(u32, Task)> {
        let conn_info = extensions.get::<VsockConnectInfo>().unwrap();
        match conn_info.peer_addr() {
            Some(addr) => self
                .task_list
                .lock()
                .await
                .get(&addr.cid())
                .map(|task| (*&addr.cid(), task.clone())),
            None => None,
        }
    }
}

/// VMServer is the integration point between Gevulot node and individual
/// Nanos VMs. It implements the gRPC interface that program running in VM
/// connects to and communicates with.
#[derive(Clone)]
pub struct VMServer {
    running_task: ShareVMRunningTaskMap,
    file_data_dir: PathBuf,
    result_sender: tokio::sync::mpsc::Sender<(Task, u32, grpc::task_result_request::Result)>,
}

impl Debug for VMServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "vmserver")
    }
}

impl VMServer {
    pub fn new(
        running_task: ShareVMRunningTaskMap,
        file_data_dir: PathBuf,
        result_sender: tokio::sync::mpsc::Sender<(Task, u32, grpc::task_result_request::Result)>,
    ) -> Self {
        VMServer {
            running_task,
            file_data_dir,
            result_sender,
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

        let reply = match self
            .running_task
            .get_running_task(request.extensions())
            .await
        {
            Some((cid, task)) => grpc::TaskResponse {
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
        let (vm_id, task) = match self
            .running_task
            .remove_task_with_extension(request.extensions())
            .await
        {
            Some(res) => res,
            None => {
                return Err(Status::new(
                    Code::Unknown,
                    format!("unknown VM address: {:?}", request.remote_addr()),
                ));
            }
        };

        tracing::trace!("VMServer submit_result task:{}, vm_id:{vm_id}", task.tx);

        let request = request.into_inner();
        let request_tx_hash: Hash = (&*request.tx_hash).into();

        let task_tx = task.tx;
        if task_tx == request_tx_hash {
            if let Some(result) = request.result {
                if let Err(err) = self.result_sender.send((task, vm_id, result)).await {
                    tracing::error!("Error during submit VM execution result:{err}");
                }
            }
        } else {
            tracing::error!(
                "submit_result different Tx_hash from vm_id:{} and request result:{}",
                task.tx.to_string(),
                request_tx_hash.to_string()
            );
        }

        let reply = grpc::TaskResultResponse { r#continue: false };
        Ok(Response::new(reply))
    }
}
