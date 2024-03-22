use crate::types::transaction;
use crate::types::transaction::ProgramData;
use crate::types::Hash;
use eyre::Result;
use serde::Deserialize;
use serde::Serialize;
use std::path::Path;
use std::path::PathBuf;

// List of folder where the different file type are stored.
pub const IMAGES_DIR: &str = "images";
pub const TX_FILES_DIR: &str = "txfiles";
pub const VM_FILES_DIR: &str = "vmfiles";

// Describe a file use by an executed task.
#[derive(Clone, Debug)]
pub struct TaskVmFile<E> {
    vm_file_path: String,
    extension: E,
}

impl<E> TaskVmFile<E> {
    pub fn vm_file_path(&self) -> &str {
        &self.vm_file_path
    }
}
impl TaskVmFile<()> {
    pub fn get_workspace_path(data_directory: &Path, tx_hash: Hash) -> PathBuf {
        PathBuf::new()
            .join(data_directory)
            .join(VM_FILES_DIR)
            .join(tx_hash.to_string())
            .join(gevulot_common::WORKSPACE_NAME)
    }
}

// Define A task file send to the VM. Extension contains the node file path.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct VmInput(String);

// Define A task file receive from the VM. Extension contains task tx hash.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct VmOutput(Hash);

// Input file of a Task. VmInput store the node file path.
impl TaskVmFile<VmInput> {
    pub fn new(vm_file_path: String, task_tx: Hash) -> Self {
        let mut file_path = Path::new(&vm_file_path);
        if file_path.is_absolute() {
            file_path = file_path.strip_prefix("/").unwrap(); // Unwrap tested in `is_absolute()`.
        }
        let mut path = PathBuf::from(TX_FILES_DIR);
        path.push(task_tx.to_string());
        path.push(file_path);
        TaskVmFile::<VmInput> {
            vm_file_path,
            extension: VmInput(path.to_str().unwrap().to_string()),
        }
    }

    pub async fn copy_file_for_vm_exe(
        &self,
        data_dir: &PathBuf,
        tx_hash: Hash,
    ) -> Result<(), String> {
        let tx_file = TaskVmFile::<VmOutput>::new(self.vm_file_path.clone(), tx_hash);
        let to = PathBuf::new()
            .join(data_dir)
            .join(tx_file.get_relatif_path());
        if let Some(parent) = to.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|err| format!("mkdir {parent:?} fail:{err}"))?;
        }
        let from = PathBuf::new().join(data_dir).join(&self.extension.0);
        tracing::debug!("TaskVmFile copy_file_for_vm_exe from:{from:?} to:{to:?}",);
        tokio::fs::copy(&from, &to)
            .await
            .map_err(|err| format!("copy file from:{from:?} to:{to:?} error:{err}"))?;

        if !tokio::fs::try_exists(&to).await.unwrap_or(false) {
            tracing::error!("copy vm file doesn't copy {to:?}",);
        }
        Ok(())
    }

    pub fn try_from_prg_data(
        tx_hash: Hash,
        parent_output_files: &[TxFile<Output>],
        value: &transaction::ProgramData,
    ) -> Result<TaskVmFile<VmInput>, String> {
        match value {
            transaction::ProgramData::Input { file_name, .. } => {
                Ok(TaskVmFile::<VmInput>::new(file_name.to_string(), tx_hash))
            }
            transaction::ProgramData::Output {
                source_program: _,
                file_name,
            } => {
                // Get the file path from the parent tx file list.
                match parent_output_files
                    .iter()
                    .find(|file| &file.name == file_name)
                {
                    Some(file) => {
                        let node_file_path =
                            file.get_relatif_path(tx_hash).to_str().unwrap().to_string();
                        Ok(TaskVmFile::<VmInput> {
                            vm_file_path: file_name.to_string(),
                            extension: VmInput(node_file_path),
                        })
                    }
                    None => Err(format!(
                        "Tx:{} program output file:{file_name} not found",
                        tx_hash,
                    )),
                }
            }
        }
    }
}

// Output file of a Task. It's a VM generated file.
// VmOutput store the hash to the task's Tx
// Output file are stored in <Task Tx hash>/<VM path>
impl TaskVmFile<VmOutput> {
    pub fn new(vm_file_path: String, task_tx: Hash) -> Self {
        TaskVmFile::<VmOutput> {
            vm_file_path,
            extension: VmOutput(task_tx),
        }
    }

    pub fn get_relatif_path(&self) -> PathBuf {
        let mut file_path = Path::new(&self.vm_file_path);
        if file_path.is_absolute() {
            file_path = file_path.strip_prefix("/").unwrap(); // Unwrap tested in `is_absolute()`.
        }

        let mut path = PathBuf::from(VM_FILES_DIR);
        path.push(self.extension.0.to_string());
        path.push(file_path);
        path
    }

    pub async fn remove_file(&self, base_path: &Path) -> std::io::Result<()> {
        let src_file_path = base_path.join(self.get_relatif_path());
        tokio::fs::remove_file(src_file_path).await
    }
}

pub async fn move_vmfile(
    source: &TaskVmFile<VmOutput>,
    dest: &TxFile<Output>,
    base_path: &Path,
    proofverif_tx_hash: Hash,
) -> Result<()> {
    // If the dest file already exist don't copy it.
    // Remove it from the VM temp file path.
    if dest.exist(base_path, proofverif_tx_hash).await {
        tracing::debug!(
            "move_vmfile: dest file already exist:{:#?}. Remove VM file:{:#?}",
            dest.get_relatif_path(proofverif_tx_hash),
            source.get_relatif_path()
        );
        source.remove_file(base_path).await.map_err(|e| e.into())
    } else {
        let src_file_path = base_path.to_path_buf().join(source.get_relatif_path());
        let dst_file_path = base_path
            .to_path_buf()
            .join(dest.get_relatif_path(proofverif_tx_hash));

        tracing::debug!(
            "move_vmfile: moving file from {:#?} to {:#?}",
            src_file_path,
            dst_file_path
        );

        // Ensure any necessary subdirectories exists.
        if let Some(parent) = dst_file_path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .expect("task file mkdir");
        }

        tokio::fs::rename(src_file_path, dst_file_path)
            .await
            .map_err(|e| e.into())
    }
}

// Describe file data that is stored in the database.
// To manipulate file on disk use the equivalent type state definition TxFile<T> or TaskVmFile<T>
#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize, sqlx::FromRow)]
pub struct DbFile {
    pub name: String,
    pub url: String,
    pub checksum: Hash,
}

// AssetFile: Use to download the file asset associated to a Tx.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct AssetFile {
    pub name: String,
    pub file_path: PathBuf,
    pub url: String,
    pub checksum: Hash,
    // Verify_exist: define if the exist() verification do a real file system verification.
    // Some file must be download even if there's already present.
    pub verify_exist: bool,
}

impl AssetFile {
    pub fn new_from_program_data(data: &ProgramData, tx_hash: Hash) -> Result<Option<Self>> {
        match data {
            ProgramData::Input {
                file_name,
                file_url,
                checksum,
            } => {
                // Verify the url is valide.
                reqwest::Url::parse(file_url)?;
                let mut file_name_path = Path::new(&file_name);
                if file_name_path.is_absolute() {
                    file_name_path = file_name_path.strip_prefix("/").unwrap(); // Unwrap tested in `is_absolute()`.
                }

                // Build file path
                let mut file_path = PathBuf::from(TX_FILES_DIR);
                file_path.push(tx_hash.to_string());
                file_path.push(file_name_path);
                Ok(Some(AssetFile {
                    name: file_name.to_string(),
                    file_path,
                    url: file_url.to_string(),
                    checksum: checksum.to_string().into(),
                    verify_exist: false,
                }))
            }
            ProgramData::Output { .. } => {
                /* ProgramData::Output as input means it comes from another
                program execution -> skip this branch. */
                Ok(None)
            }
        }
    }

    // Get relative File path for downloaded files to be saved on the node.
    pub fn get_save_path(&self) -> &Path {
        &self.file_path
    }

    // Get relative File path for downloaded files to be saved on the node.
    // The path is is <Tx Hash>/<self.name>
    pub fn get_uri(&self) -> String {
        self.get_save_path().to_str().unwrap().to_string()
    }

    pub async fn exist(&self, root_path: &Path) -> bool {
        if self.verify_exist {
            let file_path = root_path.join(self.get_save_path());
            tokio::fs::try_exists(file_path).await.unwrap_or(false)
        } else {
            false
        }
    }
}

// Type state definition of a file attached to a Transaction.
// Output: A file attached to a proof or verify Tx.
// Image: File attached to a Deploy Tx. Identify an image that are stored in the image directory.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct TxFile<E> {
    pub name: String,
    pub url: String,
    pub checksum: Hash,
    pub extention: E,
}

impl<E> TxFile<E> {
    pub fn build(name: String, url: String, checksum: Hash, extention: E) -> Self {
        TxFile {
            name,
            url,
            checksum,
            extention,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct Output;
impl TxFile<Output> {
    pub fn new(path: String, http_download_host: String, checksum: Hash) -> Self {
        TxFile::build(path, http_download_host, checksum, Output)
    }

    pub fn into_download_file(self, tx_hash: Hash) -> AssetFile {
        let relative_path = self.get_relatif_path(tx_hash);
        let url = format!("{}/{}", self.url, relative_path.to_str().unwrap());

        AssetFile {
            name: self.name,
            file_path: relative_path,
            url,
            checksum: self.checksum,
            verify_exist: true,
        }
    }

    // Relative File path for Proof or Verify Tx file.
    // The path is <Tx Hash>/<self.checksum>/<filename>
    pub fn get_relatif_path(&self, tx_hash: Hash) -> PathBuf {
        let file_name = Path::new(&self.name).file_name().unwrap_or_default();
        let mut path = PathBuf::from(TX_FILES_DIR);
        path.push(tx_hash.to_string());
        path.push(self.checksum.to_string());
        path.push(file_name);
        path
    }

    pub fn vec_to_bytes(vec: &[TxFile<Output>]) -> Result<Vec<u8>, bincode::Error> {
        bincode::serialize(vec)
    }

    pub async fn exist(&self, root_path: &Path, tx_hash: Hash) -> bool {
        let file_path = root_path.join(self.get_relatif_path(tx_hash));
        tokio::fs::try_exists(file_path).await.unwrap_or(false)
    }
}

impl From<DbFile> for TxFile<Output> {
    fn from(file: DbFile) -> Self {
        TxFile::<Output>::new(file.name, file.url, file.checksum)
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct Image(Hash);
impl TxFile<Image> {
    pub fn try_from_prg_meta_data(value: &transaction::ProgramMetadata) -> Self {
        TxFile::build(
            value.image_file_name.clone(),
            value.image_file_url.clone(),
            value.image_file_checksum.clone().into(),
            Image(value.hash),
        )
    }
}

impl From<TxFile<Image>> for AssetFile {
    fn from(file: TxFile<Image>) -> Self {
        let mut file_path = PathBuf::from(IMAGES_DIR);
        file_path.push(file.extention.0.to_string()); //Tx hash
        file_path.push(&file.name);

        AssetFile {
            name: file.name,
            file_path,
            url: file.url,
            checksum: file.checksum,
            verify_exist: false,
        }
    }
}
