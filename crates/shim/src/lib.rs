use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::time::Instant;
use std::{path::Path, thread::sleep, time::Duration};

use grpc::vm_service_client::VmServiceClient;
use tokio::runtime::Runtime;
use tokio::{io::AsyncWriteExt, sync::Mutex};
use tokio_stream::StreamExt;
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
    /// `workspace` is the file directory used for task specific file downloads.
    workspace: String,

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
            workspace: workspace.to_string(),
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

        let mut task = match task_response.result {
            Some(grpc::task_response::Result::Task(task)) => Task {
                id: task.id,
                args: task.args,
                files: task.files,
            },
            Some(grpc::task_response::Result::Error(_err)) => {
                todo!("construct proper error types from TaskError")
            }
            None => return Ok(None),
        };

        let mut files = vec![];
        for file in task.files {
            let path = self
                .rt
                .block_on(self.download_file(task.id.clone(), file))?;
            files.push(path);
        }
        task.files = files;

        Ok(Some(task))
    }

    fn submit_result(&mut self, result: &TaskResult) -> Result<bool> {
        // TODO(tuommaki): Implement result file transformation!
        let task_result_req = grpc::TaskResultRequest {
            result: Some(grpc::task_result_request::Result::Task(grpc::TaskResult {
                id: result.id.clone(),
                data: result.data.clone(),
                files: result
                    .files
                    .iter()
                    .map(|f| grpc::File {
                        path: f.to_string(),
                        data: std::fs::read(f).expect("result file read"),
                    })
                    .collect(),
            })),
        };

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

    /// download_file asks gRPC server for file with a `name` and writes it to
    /// `workspace`.
    async fn download_file(&self, task_id: TaskId, name: String) -> Result<String> {
        let file_req = grpc::FileRequest {
            task_id: task_id.clone(),
            path: name.clone(),
        };

        let file_path = Path::new(&self.workspace).join(task_id).join(name.clone());
        if let Some(parent) = file_path.parent() {
            if let Ok(false) = tokio::fs::try_exists(parent).await {
                tokio::fs::create_dir_all(parent).await?;
            }
        }
        let path = match file_path.into_os_string().into_string() {
            Ok(path) => path,
            Err(e) => panic!("failed to construct path for a file to write: {:?}", e),
        };

        // Ensure any necessary subdirectories exists.
        if let Some(parent) = Path::new(&path).parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .expect("task file mkdir");
        }

        let mut stream = self
            .client
            .lock()
            .await
            .get_file(file_req)
            .await?
            .into_inner();

        let out_file = tokio::fs::File::create(path.clone()).await?;
        let mut writer = tokio::io::BufWriter::new(out_file);

        let mut total_bytes = 0;

        while let Some(Ok(grpc::FileResponse { result: resp })) = stream.next().await {
            match resp {
                Some(grpc::file_response::Result::Chunk(file_chunk)) => {
                    total_bytes += file_chunk.data.len();
                    writer.write_all(file_chunk.data.as_ref()).await?;
                }
                Some(grpc::file_response::Result::Error(err)) => {
                    panic!("error while fetching file {}: {}", name, err)
                }
                None => {
                    println!("stream broken");
                    break;
                }
            }
        }
        writer.flush().await?;

        println!("downloaded {} bytes for {}", &total_bytes, &name);

        Ok(path)
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
pub fn run(callback: impl Fn(&Task) -> Result<TaskResult>) -> Result<()> {
    let mut client = GRPCClient::new(8080, "/workspace")?;

    loop {
        let task = match client.get_task() {
            Ok(Some(task)) => task,
            Ok(None) => {
                sleep(Duration::from_secs(1));
                continue;
            }
            Err(err) => {
                println!("get_task(): {}", err);
                sleep(Duration::from_secs(5));
                continue;
            }
        };

        let result = callback(&task)?;

        let should_continue = client.submit_result(&result)?;
        if !should_continue {
            break;
        }
    }

    Ok(())
}
