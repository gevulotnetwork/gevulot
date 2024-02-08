use crate::types::transaction;
use crate::types::Hash;
use eyre::Result;
use serde::Deserialize;
use serde::Serialize;
use std::path::Path;
use std::path::PathBuf;

//describe file data for a Tx
#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize, sqlx::FromRow)]
pub struct TxFile {
    pub name: String,
    pub url: String,
    pub checksum: Hash,
}

impl TxFile {
    pub fn try_from_prg_data(
        value: &transaction::ProgramData,
    ) -> Result<Option<Self>, &'static str> {
        let file = match value {
            transaction::ProgramData::Input {
                file_name,
                file_url,
                checksum,
            } => Some(TxFile {
                url: file_url.clone(),
                name: file_name.clone(),
                checksum: checksum.clone().into(),
            }),
            transaction::ProgramData::Output {
                source_program: _,
                file_name: _,
            } => {
                //Output file are not use by Tx management.
                None
            }
        };

        Ok(file)
    }

    pub fn to_asset_file(self, tx_hash: Hash) -> AssetFile {
        AssetFile::from_txfile(self, tx_hash)
    }
}

pub fn vec_txfile_to_bytes(vec: &[TxFile]) -> Result<Vec<u8>, bincode::Error> {
    bincode::serialize(vec)
}

//describe file data to calculate the file path for read/write
#[derive(Clone, Debug)]
pub struct AssetFile {
    //true if the file is an image.
    pub image: bool,
    pub tx: Hash,
    pub name: String,
    pub url: String,
    pub checksum: Hash,
}

impl AssetFile {
    fn from_txfile(file: TxFile, tx_hash: Hash) -> Self {
        AssetFile {
            image: false,
            url: file.url,
            name: file.name,
            tx: tx_hash,
            checksum: file.checksum,
        }
    }

    pub fn get_relatif_path(&self) -> PathBuf {
        let file_name = Path::new(&self.name).file_name().unwrap();
        let mut path = if self.image {
            PathBuf::from("images").join(self.tx.to_string())
        } else {
            PathBuf::from(self.tx.to_string())
        };
        path.push(file_name);
        path
    }

    pub fn try_from_prg_meta_data(
        value: &transaction::ProgramMetadata,
    ) -> Result<Self, &'static str> {
        Ok(AssetFile {
            image: true,
            url: value.image_file_url.clone(),
            name: value.name.clone(),
            tx: value.hash,
            checksum: value.image_file_checksum.clone().into(),
        })
    }
}

pub async fn move_vmfile(source: &VmFile, dest: &AssetFile, base_path: &Path) -> Result<()> {
    let src_file_path = base_path.to_path_buf().join(&source.get_vmrelatif_path());
    let dst_file_path = base_path.to_path_buf().join(&dest.get_relatif_path());

    tracing::debug!(
        "moving file from {:#?} to {:#?}",
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

//describe file data to calculate the file path for read/write
#[derive(Clone, Debug)]
pub struct VmFile {
    pub task_id: Hash,
    pub name: String,
}

impl VmFile {
    pub fn get_vmrelatif_path(&self) -> PathBuf {
        let name_path = Path::new(&self.name);

        //remove root from the path because PathBuf push replace when using abs path.
        let file_path = if name_path.is_absolute() {
            &self.name[1..]
        } else {
            &self.name[..]
        };
        let mut path = PathBuf::from(self.task_id.to_string());
        path.push(file_path);
        path
    }
}
