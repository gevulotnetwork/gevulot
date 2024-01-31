use std::{
    any::Any,
    collections::HashMap,
    fs::File,
    path::Path,
    process::{Child, Command, Stdio},
    sync::Arc,
    time::{Duration, Instant},
};

use async_trait::async_trait;
use eyre::Result;
use qapi::{
    futures::{QapiStream, QmpStreamTokio},
    qmp,
    qmp::StatusInfo,
};
use rand::{self, distributions::Alphanumeric, Rng};
use serde_json::json;
use tokio::{
    io::{ReadHalf, WriteHalf},
    net::{TcpStream, ToSocketAddrs},
    sync::Mutex,
    time::sleep,
};
use tokio_vsock::{Incoming, VsockConnectInfo, VsockListener};
use tonic::Extensions;
use vsock::get_local_cid;

use super::{vm_server::ProgramRegistry, Provider, VMClient, VMHandle, VMId};
use crate::{
    cli::Config,
    nanos,
    types::{Hash, Program},
    vmm::ResourceRequest,
};

const IMAGES_DIR: &str = "images";

impl VMId for u32 {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn eq(&self, x: Arc<dyn VMId>) -> bool {
        *self == u32_from_any(x.as_any())
    }
}

fn u32_from_any(x: &dyn Any) -> u32 {
    match x.downcast_ref::<u32>() {
        Some(cid) => *cid,
        None => panic!("incompatible VMId type"),
    }
}

pub struct QEMUVMHandle {
    child: Option<Child>,
    cid: u32,
    program_id: Hash,
    workspace_volume_label: String,
    //qmp: Arc<Mutex<Qmp>>,
}

pub struct Qemu {
    config: Arc<Config>,
    cid_allocations: Vec<u32>,
    vm_registry: HashMap<u32, QEMUVMHandle>,
}

impl Qemu {
    pub fn new(config: Arc<Config>) -> Self {
        Qemu {
            config,
            cid_allocations: vec![],
            vm_registry: HashMap::new(),
        }
    }

    fn allocate_cid(&mut self) -> u32 {
        loop {
            let cid = rand::random::<u32>();

            if cid < 3 {
                // CIDs 0, 1 and 2 are reserved.
                continue;
            }

            if self.cid_allocations.iter().any(|&x| x == cid) {
                // Generated CID found from existing allocations.
                continue;
            };

            self.cid_allocations.push(cid);
            return cid;
        }
    }

    fn release_cid(&mut self, cid: u32) {
        if let Some(idx) = self.cid_allocations.iter().position(|&x| x == cid) {
            self.cid_allocations.remove(idx);
            self.vm_registry.remove(&cid);
        }
    }

    pub fn vm_server_listener(&self) -> Result<Incoming> {
        let cid = match get_local_cid() {
            Ok(cid) => cid,
            Err(ref e) if e.kind() == std::io::ErrorKind::NotFound => {
                eprintln!(
                    "error: /dev/vsock not found; ensure 'vhost_vsock' kernel module is loaded"
                );
                std::process::exit(-1);
            }
            Err(e) => panic!("can't get local VSOCK CID: {}", e),
        };
        let listener = VsockListener::bind(cid, self.config.vsock_listen_port).expect("bind");
        Ok(listener.incoming())
    }
}

impl ProgramRegistry for Qemu {
    fn find_by_req(&mut self, extensions: &Extensions) -> Option<(Hash, Arc<dyn VMId>)> {
        let conn_info = extensions.get::<VsockConnectInfo>().unwrap();
        match conn_info.peer_addr() {
            Some(addr) => {
                self.vm_registry
                    .get(&addr.cid())
                    .map(|handle| -> (Hash, Arc<dyn VMId>) {
                        (handle.program_id, Arc::new(addr.cid()))
                    })
            }
            None => None,
        }
    }
}

#[async_trait]
impl Provider for Qemu {
    async fn start_vm(&mut self, program: Program, req: ResourceRequest) -> Result<VMHandle> {
        // TODO:
        //  - Builder to construct QEMU flags
        //  - Handle GPUs

        let img_file = Path::new(&self.config.data_directory)
            .join(IMAGES_DIR)
            .join(program.hash.to_string())
            .join(program.image_file_name);

        let workspace_volume_label: String = rand::thread_rng()
            .sample_iter(&Alphanumeric)
            .take(16)
            .map(char::from)
            .collect::<String>()
            .to_lowercase();

        // XXX: This isn't async and will call out to `ops` for now.
        tracing::debug!("creating workspace volume for the VM");
        let workspace_file =
            nanos::volume::create(&self.config.data_directory, &workspace_volume_label, "2g")?
                .into_os_string();
        let workspace_file = workspace_file.to_str().expect("workspace volume path");
        tracing::debug!("workspace volume created");

        let cpus = req.cpus;
        let mem_req = req.mem;
        let cid = self.allocate_cid();

        // Random unprivileged port computed from allocated CID.
        // If there is collision with the port, QEMU startup will fail and
        // watchdog will reap it.
        let qmp_port = (cid % 64512) + 1024;

        let program_id = program.hash;
        let qemu_vm_handle = QEMUVMHandle {
            child: None,
            cid,
            program_id,
            workspace_volume_label,
            //qmp: Arc::new(Mutex::new(qmp)),
        };

        // Must VM must be registered before start, because when the VM starts,
        // the program in it starts immediately and queries for task, which
        // requires the VM to be registered for identification.
        self.vm_registry.insert(cid, qemu_vm_handle);

        // Update the child process field.
        let qemu_vm_handle = &mut self.vm_registry.get_mut(&cid).unwrap();
        let mut cmd = Command::new("/usr/bin/qemu-system-x86_64");

        cmd.args(["-machine", "q35"])
            .args([
                "-device",
                "pcie-root-port,port=0x10,chassis=1,id=pci.1,bus=pcie.0,multifunction=on,addr=0x3",
            ])
            .args([
                "-device",
                "pcie-root-port,port=0x11,chassis=2,id=pci.2,bus=pcie.0,addr=0x3.0x1",
            ])
            .args([
                "-device",
                "pcie-root-port,port=0x12,chassis=3,id=pci.3,bus=pcie.0,addr=0x3.0x2",
            ])
            // Register 2 hard drives via SCSI
            .args(["-device", "virtio-scsi-pci,bus=pci.2,addr=0x0,id=scsi0"])
            .args(["-device", "scsi-hd,bus=scsi0.0,drive=hd0"])
            //.args(["-device", "scsi-hd,bus=scsi0.0,drive=hd1"])
            .args(["-vga", "none"])
            // CPUS
            .args(["-smp", &cpus.to_string()])
            .args(["-device", "isa-debug-exit"])
            // MEMORY
            .args(["-m", &format!("{mem_req}M")])
            .args(["-device", "virtio-rng-pci"])
            .args(["-machine", "accel=kvm:tcg"])
            .args(["-cpu", "host"])
            .arg("-no-reboot")
            .arg("-no-shutdown")
            .args(["-cpu", "max"])
            // IMAGE FILE
            .args([
                "-drive",
                &format!(
                    "file={},format=raw,if=none,id=hd0,readonly=on",
                    &img_file.into_os_string().into_string().unwrap(),
                ),
            ])
            // WORKSPACE FILE
            /*
            .args([
                "-drive",
                &format!("file={},format=raw,if=none,id=hd1", &workspace_file),
            ])*/
            // NETWORKING
            .args([
                "-device",
                "virtio-net,bus=pci.3,addr=0x0,netdev=n0,mac=8e:97:45:7c:fb:3d",
            ])
            .args(["-netdev", "user,id=n0"])
            .args(["-display", "none"])
            .args(["-serial", "stdio"])
            // VSOCK
            .args(["-device", &format!("vhost-vsock-pci,guest-cid={cid}")])
            // QMP
            .args(["-qmp", &format!("tcp:localhost:{qmp_port},server")]);

        // TODO: When GPU argument handling is refactored, this should be fixed as well.
        if self.config.gpu_devices.is_some() {
            cmd.args(parse_gpu_devices_into_qemu_params(
                self.config.gpu_devices.as_ref().unwrap(),
            ));
        }

        // Setup stdout & stderr log to VM execution.
        {
            let log_dir_path = Path::new(&self.config.log_directory).join(program.hash.to_string());
            std::fs::create_dir_all(&log_dir_path)?;
            let stdout = File::options()
                .create(true)
                .append(true)
                .open(Path::new(&log_dir_path).join("stdout.log"))?;
            let stderr = File::options()
                .create(true)
                .append(true)
                .open(Path::new(&log_dir_path).join("stderr.log"))?;
            cmd.stdout(Stdio::from(stdout));
            cmd.stderr(Stdio::from(stderr));
        }

        tracing::info!("starting QEMU. args:\n{:#?}\n", cmd.get_args());

        qemu_vm_handle.child = Some(cmd.spawn().expect("failed to start VM"));

        let start_time = Instant::now();

        // Reconnect until the VM starts.
        let qmp_client = {
            let mut client = None;
            let mut retry_count = 0;
            while client.is_none() {
                if retry_count > 100 {
                    // If we can't start QEMU, there's no point running the node
                    // and the best way to capture operator's attention is to panic.
                    panic!("failed to start QEMU; aborting.");
                }

                match Qmp::new(format!("localhost:{qmp_port}")).await {
                    Ok(c) => client = Some(c),
                    Err(_) => {
                        retry_count += 1;
                        sleep(Duration::from_millis(10)).await;
                    }
                };
            }
            client.unwrap()
        };

        // Attach the workspace volume.
        let err_add = qmp_client.blockdev_add("workspace", workspace_file).await;
        if err_add.is_err() {
            tracing::error!("blockdev_add failed: {:?}", err_add);
        }

        let err_add = qmp_client.device_add("workspace", 1).await;
        if err_add.is_err() {
            tracing::error!("device_add failed: {:?}", err_add);
        }

        qmp_client.system_reset().await?;

        Ok(VMHandle {
            start_time,
            vm_id: Arc::new(cid),
            vm_client: Arc::new(qmp_client),
        })
    }

    fn stop_vm(&mut self, vm: VMHandle) -> Result<()> {
        if let Some(qemu_vm_handle) = self.vm_registry.get_mut(&u32_from_any(vm.vm_id.as_any())) {
            drop(vm);
            qemu_vm_handle
                .child
                .as_mut()
                .unwrap()
                .kill()
                .expect("failed to kill VM");

            // GC ephemeral workspace volume.
            // XXX: This isn't async and will call out to `ops`.
            nanos::volume::delete(&qemu_vm_handle.workspace_volume_label)?;

            let cid = qemu_vm_handle.cid;
            self.release_cid(cid);

            Ok(())
        } else {
            todo!("create error type for VM NOT FOUND");
        }
    }

    fn prepare_image(&mut self, _program: Program, _image: &std::path::Path) -> Result<()> {
        // QEMU provider doesn't need to do anything for the image.
        // It uses the local file as-is.
        Ok(())
    }
}

struct Qmp {
    #[allow(clippy::type_complexity)]
    stream: Mutex<
        QapiStream<QmpStreamTokio<ReadHalf<TcpStream>>, QmpStreamTokio<WriteHalf<TcpStream>>>,
    >,
}

impl Qmp {
    async fn new(addr: impl ToSocketAddrs) -> Result<Self> {
        let stream = QmpStreamTokio::open_tcp(addr).await?;
        tracing::debug!("QMP: {:#?}", stream.capabilities);
        let stream = stream.negotiate().await?;
        let stream = Mutex::new(stream);

        Ok(Qmp { stream })
    }

    async fn query_status(&self) -> Result<StatusInfo> {
        self.stream
            .lock()
            .await
            .execute(qmp::query_status {})
            .await
            .map_err(|e| e.into())
    }

    async fn blockdev_add(&self, node_name: &str, device_file: &str) -> Result<()> {
        self.stream
            .lock()
            .await
            .execute(qmp::blockdev_add(qmp::BlockdevOptions::raw {
                base: qmp::BlockdevOptionsBase {
                    detect_zeroes: None,
                    cache: None,
                    discard: None,
                    force_share: None,
                    auto_read_only: None,
                    node_name: Some(node_name.to_string()),
                    read_only: None,
                },
                raw: qmp::BlockdevOptionsRaw {
                    base: qmp::BlockdevOptionsGenericFormat {
                        file: qmp::BlockdevRef::definition(Box::new(qmp::BlockdevOptions::file {
                            base: qmp::BlockdevOptionsBase {
                                auto_read_only: None,
                                cache: None,
                                detect_zeroes: None,
                                discard: None,
                                force_share: None,
                                node_name: None, //Some(format!("{node_name}")),
                                read_only: Some(false),
                            },
                            file: qmp::BlockdevOptionsFile {
                                aio: None,
                                filename: device_file.to_string(),
                                aio_max_batch: None,
                                drop_cache: None,
                                locking: None,
                                pr_manager: None,
                                x_check_cache_dropped: None,
                            },
                        })),
                    },
                    offset: None,
                    size: None,
                },
            }))
            .await
            .map_err(|e| e.into())
            .map(|_| ())
    }

    async fn device_add(&self, node_name: &str, disk_id: u8) -> Result<()> {
        let mut args = serde_json::map::Map::new();
        args.insert(String::from("drive"), json!(node_name));
        args.insert(
            String::from("device_id"),
            json!(format!("persistent-disk-{disk_id}")),
        );
        self.stream
            .lock()
            .await
            .execute(qmp::device_add {
                bus: Some("scsi0.0".to_string()),
                id: Some(node_name.to_string()),
                driver: "scsi-hd".to_string(),
                arguments: args,
            })
            .await
            .map_err(|e| e.into())
            .map(|_| ())
    }

    async fn system_reset(&self) -> Result<()> {
        self.stream
            .lock()
            .await
            .execute(qmp::system_reset {})
            .await
            .map_err(|e| e.into())
            .map(|_| ())
    }
}

#[async_trait::async_trait]
impl VMClient for Qmp {
    async fn is_alive(&self) -> Result<bool> {
        self.query_status().await.map(|status| status.running)
    }
}

//
// TODO: This is totally wrong place to put CLI argument parsing, but done
//       this way to save some time.
//
// The GPU arguments should be completely refactored. Possibly even removed
// and replaced with automatic detection (by filtering PCI devices that are
// bound with vfio_pci driver).
fn parse_gpu_devices_into_qemu_params(arg: &str) -> Vec<String> {
    let devices: Vec<&str> = arg.split(',').collect();
    let mut params = vec![];
    for device in devices {
        params.push("-device".to_string());
        params.push(format!("vfio-pci,rombar=0,host={}", device));
    }
    params
}
