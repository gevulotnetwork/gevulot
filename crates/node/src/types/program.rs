use serde::{Deserialize, Serialize};

use super::{
    hash::{deserialize_hash_from_json, Hash},
    transaction,
};

use crate::vmm;

#[derive(Clone, Debug, Default, Deserialize, Serialize, sqlx::FromRow)]
pub struct Program {
    #[serde(deserialize_with = "deserialize_hash_from_json")]
    pub hash: Hash,
    pub name: String,
    pub image_file_name: String,
    pub image_file_url: String,
    pub image_file_checksum: String,
    #[sqlx(skip)]
    pub limits: vmm::ResourceRequest,
}

impl From<transaction::ProgramMetadata> for Program {
    fn from(value: transaction::ProgramMetadata) -> Self {
        Program {
            hash: value.hash,
            name: value.name,
            image_file_name: value.image_file_name,
            image_file_url: value.image_file_url,
            image_file_checksum: value.image_file_checksum,
            limits: Default::default(),
        }
    }
}

impl From<Program> for transaction::ProgramMetadata {
    fn from(value: Program) -> Self {
        transaction::ProgramMetadata {
            name: value.name,
            hash: value.hash,
            image_file_name: value.image_file_name,
            image_file_url: value.image_file_url,
            image_file_checksum: value.image_file_checksum,
        }
    }
}
