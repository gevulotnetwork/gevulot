use self::grpc::{
    FileChunk, FileData, FileMetadata, GenericResponse, TaskResponse, TaskResultRequest,
};
use crate::types::file::{File, Vm};
use crate::types::Hash;
use crate::types::Task;
use crate::vmm::vm_server::grpc::file_data;
use async_trait::async_trait;
use eyre::Result;
use grpc::vm_service_server::VmService;
use grpc::{GetFileRequest, TaskRequest, TaskResultResponse};
use std::fmt::Debug;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::sync::{mpsc, Mutex};
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Code, Extensions, Request, Response, Status, Streaming};

use super::VMId;

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
    async fn get_pending_task(&self, program: Hash, vm_id: Arc<dyn VMId>) -> Option<Task>;
    async fn get_running_task(&self, vm_id: Arc<dyn VMId>) -> Option<Task>;
    async fn submit_result(
        &self,
        program: Hash,
        vm_id: Arc<dyn VMId>,
        result: grpc::task_result_request::Result,
    ) -> Result<(), String>;
}

/// ProgramRegistry defines interface that `VMServer` uses to identify which
/// `Program` sent corresponding request.
pub trait ProgramRegistry: Send {
    fn find_by_req(&mut self, extensions: &Extensions) -> Option<(Hash, Arc<dyn VMId>)>;
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
    type GetFileStream = ReceiverStream<Result<FileData, Status>>;

    #[tracing::instrument]
    async fn get_task(
        &self,
        request: Request<TaskRequest>,
    ) -> Result<Response<TaskResponse>, Status> {
        tracing::info!("request for task: {:?}", request);
        let (program, vm_id) = self
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

        let reply = match self.task_source.get_pending_task(program, vm_id).await {
            Some(task) => grpc::TaskResponse {
                result: Some(grpc::task_response::Result::Task(grpc::Task {
                    id: task.tx.to_string(),
                    name: task.name.to_string(),
                    args: task.args,
                    files: task.files.into_iter().map(|x| x.vm_file_path).collect(),
                })),
            },
            None => grpc::TaskResponse {
                result: Some(grpc::task_response::Result::Error(
                    grpc::TaskError::Unavailable.into(),
                )),
            },
        };
        tracing::trace!("VMServer get_task reply: {:?}", reply);

        Ok(Response::new(reply))
    }

    #[tracing::instrument]
    async fn get_file(
        &self,
        request: Request<GetFileRequest>,
    ) -> Result<Response<Self::GetFileStream>, Status> {
        tracing::info!("request for file: {:?}", request);

        let (program, vm_id) = self
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

        let task = self
            .task_source
            .get_running_task(vm_id)
            .await
            .ok_or_else(|| Status::new(Code::NotFound, "couldn't find running task for request"))?;

        tracing::trace!("get_file found task: {:#?}", task);

        let req = request.into_inner();
        //get VM file associated to this task file
        let vm_file = task
            .files
            .iter()
            .find(|file| file.vm_file_path == req.path)
            .ok_or_else(|| Status::new(Code::NotFound, "couldn't get task file"))?;

        tracing::trace!("get_file found vm_file: {vm_file:?}");

        let mut file_stream = vm_file
            .open_task_file(&self.file_data_dir)
            .await
            .map_err(|err| {
                Status::new(
                    Code::NotFound,
                    format!("couldn't not open task file  :{err}"),
                )
            })?;

        tracing::trace!("get_file file_stream created");

        let (tx, rx) = mpsc::channel(4);
        tokio::spawn({
            async move {
                let mut buf: [u8; DATA_STREAM_CHUNK_SIZE] = [0; DATA_STREAM_CHUNK_SIZE];

                loop {
                    match file_stream.read(&mut buf).await {
                        Ok(0) => return Ok(()),
                        Ok(n) => {
                            if let Err(e) = tx
                                .send(Ok(grpc::FileData {
                                    result: Some(file_data::Result::Chunk(grpc::FileChunk {
                                        data: buf[..n].to_vec(),
                                    })),
                                }))
                                .await
                            {
                                //                                tracing::error!("send {} bytes from file {}: {}", n, &req.path, &e);
                                tracing::error!("send {} bytes from file: {}", n, &e);
                                break;
                            }
                        }
                        Err(e) => return Err(e),
                    }
                }

                Ok(())
            }
        });

        Ok(Response::new(ReceiverStream::new(rx)))
    }

    #[tracing::instrument]
    async fn submit_file(
        &self,
        request: Request<Streaming<FileData>>,
    ) -> Result<Response<GenericResponse>, Status> {
        let (program, vm_id) = match self
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

        let mut stream = request.into_inner();
        let mut file: Option<tokio::io::BufWriter<tokio::fs::File>> = None;

        while let Ok(Some(grpc::FileData { result: data })) = stream.message().await {
            match data {
                Some(grpc::file_data::Result::Metadata(FileMetadata { task_id, path })) => {
                    let vmfile = File::<Vm>::new(path, Hash::default(), task_id);
                    let relative_file_path = vmfile.get_relatif_path();

                    let file_path = PathBuf::new()
                        .join(&self.file_data_dir)
                        .join(relative_file_path);
                    tracing::trace!("submit_file saved in:{file_path:#?}");

                    // Ensure any necessary subdirectories exists.
                    if let Some(parent) = file_path.parent() {
                        tokio::fs::create_dir_all(parent)
                            .await
                            .expect("task file mkdir");
                    }

                    let fd = tokio::fs::File::create(file_path).await?;
                    file = Some(tokio::io::BufWriter::new(fd));
                }
                Some(grpc::file_data::Result::Chunk(FileChunk { data })) => match file.as_mut() {
                    Some(fd) => {
                        if let Err(err) = fd.write_all(data.as_slice()).await {
                            tracing::error!("error while writing to file: {}", err);
                            return Err(Status::new(
                                Code::Internal,
                                "failed to write file".to_string(),
                            ));
                        } else {
                            tracing::debug!("{} bytes received & written to file", data.len());
                        }
                    }
                    None => {
                        tracing::error!("received None from client on submit_file stream");
                        return Err(Status::new(
                            Code::InvalidArgument,
                            "file data sent before metadata; aborting".to_string(),
                        ));
                    }
                },
                Some(grpc::file_data::Result::Error(code)) => {
                    tracing::error!("error from client: {code}");
                    return Err(Status::new(
                        Code::Aborted,
                        format!("file transfer aborted by client; error code: {code}"),
                    ));
                }
                None => {
                    tracing::error!("FileData message with None as a body");
                    return Err(Status::new(
                        Code::InvalidArgument,
                        "file data sent without body".to_string(),
                    ));
                }
            }
        }

        if file.is_some() {
            if let Err(err) = file.unwrap().flush().await {
                tracing::error!("failed to flush file: {}", err);
                return Err(Status::new(
                    Code::Internal,
                    "failed to flush file writes".to_string(),
                ));
            };
        }

        Ok(Response::new(GenericResponse {
            success: true,
            message: String::from("file received"),
        }))
    }

    #[tracing::instrument]
    async fn submit_result(
        &self,
        request: Request<TaskResultRequest>,
    ) -> Result<Response<TaskResultResponse>, Status> {
        let (program, vm_id) = match self
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
            program.to_string()
        );

        let result = request.into_inner().result;

        if let Some(result) = result {
            if let Err(err) = self.task_source.submit_result(program, vm_id, result).await {
                tracing::error!("Error during submit VM execution result:{err}");
            }
        }

        let reply = grpc::TaskResultResponse { r#continue: false };
        Ok(Response::new(reply))
    }
}
