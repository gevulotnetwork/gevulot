use grpc::vm_service_client::VmServiceClient;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::time::Instant;
use std::{path::Path, thread::sleep, time::Duration};
use tokio::runtime::Runtime;
use tokio::sync::Mutex;
use tokio_vsock::VsockStream;
use tonic::transport::{Channel, Endpoint, Uri};
use tower::service_fn;
use vsock::VMADDR_CID_HOST;

mod grpc {
    tonic::include_proto!("vm_service");
}

/// MOUNT_TIMEOUT is maximum amount of time to wait for workspace mount to be
/// present in /proc/mounts.
const MOUNT_TIMEOUT: Duration = Duration::from_secs(30);

pub const WORKSPACE_PATH: &str = "/workspace";
pub const WORKSPACE_NAME: &str = "workspace";

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
pub type TaskId = String;

#[derive(Debug)]
pub struct Task {
    pub id: TaskId,
    pub args: Vec<String>,
    pub files: Vec<String>,
}

#[derive(Debug)]
pub struct TaskResult {
    id: TaskId,
    data: Vec<u8>,
    files: Vec<String>,
}

impl Task {
    pub fn result(&self, data: Vec<u8>, files: Vec<String>) -> Result<TaskResult> {
        Ok(TaskResult {
            id: self.id.clone(),
            data,
            files,
        })
    }

    pub fn get_task_files_path<'a>(&'a self, workspace: &str) -> Vec<(&'a str, PathBuf)> {
        self.files
            .iter()
            .map(|name| {
                let path = Path::new(workspace).join(&self.id).join(name);
                (name.as_str(), path)
            })
            .collect()
    }
}

struct GRPCClient {
    // `workspace` is the file directory used for task specific file downloads.
    // workspace: String,
    client: Mutex<VmServiceClient<Channel>>,
    rt: Runtime,
}

impl GRPCClient {
    fn new(port: u32, workspace: &str) -> Result<Self> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;

        let client = rt.block_on(GRPCClient::connect(port))?;

        println!("waiting for {workspace} mount to be present");
        let beginning = Instant::now();
        loop {
            if beginning.elapsed() > MOUNT_TIMEOUT {
                panic!("{} mount timeout", workspace);
            }

            if mount_present(workspace)? {
                println!("{workspace} mount is now present");
                break;
            }

            sleep(Duration::from_secs(1));
        }

        Ok(GRPCClient {
            //    workspace: workspace.to_string(),
            client: Mutex::new(client),
            rt,
        })
    }

    async fn connect(port: u32) -> Result<VmServiceClient<Channel>> {
        let channel = Endpoint::try_from(format!("http://[::]:{}", port))?
            .connect_with_connector(service_fn(move |_: Uri| {
                // Connect to a VSOCK server
                VsockStream::connect(VMADDR_CID_HOST, port)
            }))
            .await?;

        Ok(grpc::vm_service_client::VmServiceClient::new(channel))
    }

    fn get_task(&mut self) -> Result<Option<Task>> {
        let task_response = self
            .rt
            .block_on(async {
                self.client
                    .lock()
                    .await
                    .get_task(grpc::TaskRequest {})
                    .await
            })?
            .into_inner();

        let task = match task_response.result {
            Some(grpc::task_response::Result::Task(task)) => Task {
                id: task.tx_hash,
                args: task.args,
                files: task.files,
            },
            Some(grpc::task_response::Result::Error(code)) => {
                return Err(Box::new(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    format!("get_task() resulted with error code {}", code),
                )));
            }
            None => return Ok(None),
        };

        Ok(Some(task))
    }

    fn submit_file(&mut self, file_path: &str) -> Result<[u8; 32]> {
        let mut hasher = blake3::Hasher::new();
        let fd =
            // Add the file to the error because str file error doesn't indicate the file in error.
            std::fs::File::open(file_path).map_err(|err| format!("{err} for file:{file_path}"))?;
        hasher.update_reader(fd)?;
        let checksum = hasher.finalize();
        Ok(checksum.into())
    }

    fn submit_result(&mut self, task_id: String, result: Result<TaskResult>) -> Result<bool> {
        let task_result_req = result
            .and_then(|result| {
                // Manage result files if any.
                let files = result
                    .files
                    .into_iter()
                    .map(|file| {
                        self.submit_file(&file).map(|checksum| crate::grpc::File {
                            path: file,
                            checksum: checksum.to_vec(),
                        })
                    })
                    .collect::<Result<Vec<_>>>()?;
                Ok(grpc::TaskResultRequest {
                    tx_hash: result.id,
                    result: Some(grpc::task_result_request::Result::Task(grpc::TaskResult {
                        data: result.data,
                        files,
                    })),
                })
            })
            .unwrap_or_else(|err| {
                println!("An error occurs during client execution:{:?}", err);
                grpc::TaskResultRequest {
                    tx_hash: task_id,
                    result: Some(grpc::task_result_request::Result::Error(
                        grpc::TaskError::Failed.into(),
                    )),
                }
            });

        let response = self
            .rt
            .block_on(async {
                self.client
                    .lock()
                    .await
                    .submit_result(task_result_req)
                    .await
            })?
            .into_inner();

        Ok(response.r#continue)
    }
}

fn mount_present(mount_point: &str) -> Result<bool> {
    let file = File::open("/proc/mounts")?;
    let reader = BufReader::new(file);

    for line in reader.lines() {
        let line = line.expect("read /proc/mounts");
        if line.contains(mount_point) {
            return Ok(true);
        }
    }

    Ok(false)
}

/// run function takes `callback` that is invoked with executable `Task` and
/// which is expected to return `TaskResult`.
pub fn run(callback: impl Fn(Task) -> Result<TaskResult>) -> Result<()> {
    let mut client = GRPCClient::new(8080, WORKSPACE_PATH)?;

    loop {
        let task = match client.get_task() {
            Ok(Some(task)) => task,
            Ok(None) => {
                sleep(Duration::from_secs(1));
                continue;
            }
            Err(err) => {
                println!("get_task(): {}", err);
                return Err(err);
            }
        };

        let task_id = task.id.clone();
        let result = callback(task);

        let should_continue = match client.submit_result(task_id, result) {
            Ok(res) => res,
            Err(err) => {
                println!("An error occurs during submit_result {err}");
                return Err(err);
            }
        };
        if !should_continue {
            break;
        }
    }

    Ok(())
}
