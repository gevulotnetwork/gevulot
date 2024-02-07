use crate::types::transaction;
use crate::types::Hash;
use serde::Deserialize;
use serde::Serialize;
use std::path::Path;
use std::path::PathBuf;

#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize, sqlx::FromRow)]
pub struct AssetFile {
    #[serde(skip_serializing, skip_deserializing)]
    pub tx: Hash,
    pub name: String,
    pub url: String,
}

impl AssetFile {
    pub fn get_relatif_path(&self) -> PathBuf {
        //TODO use file name because the checksum is not saved in the Db.
        //checksum is more secure to name collision.
        let file_name = Path::new(&self.name).file_name().unwrap();
        PathBuf::new().join(self.tx.to_string()).join(file_name)
    }

    pub fn try_from_prg_meta_data(
        value: &transaction::ProgramMetadata,
        tx_hash: Hash,
    ) -> Result<(Self, Hash), &'static str> {
        Ok((
            AssetFile {
                url: value.image_file_url.clone(),
                name: value.name.clone(),
                tx: tx_hash,
            },
            value.image_file_checksum.clone().into(),
        ))
    }

    pub fn try_from_prg_data(
        value: &transaction::ProgramData,
        tx_hash: Hash,
    ) -> Result<Self, &'static str> {
        let file = match value {
            transaction::ProgramData::Input {
                file_name,
                file_url,
                ..
            } => AssetFile {
                url: file_url.clone(),
                name: file_name.clone(),
                tx: tx_hash,
            },
            transaction::ProgramData::Output {
                source_program,
                file_name,
            } => {
                //pick from workflow::workflow_step_to_task
                todo!()
                // // Make record of file that needs transfer from source tx to current tx's files.
                // file_transfers.push((*source_program, file_name.clone()));

                // AssetFile {
                //     tx,
                //     name: file_name.clone(),
                //     url: "".to_string(),
                // }
            }
        };

        Ok(file)
    }
}
