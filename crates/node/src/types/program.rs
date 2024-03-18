use serde::{Deserialize, Serialize};

use super::{
    hash::{deserialize_hash_from_json, Hash},
    transaction,
};

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize, sqlx::FromRow)]
pub struct ResourceRequest {
    #[sqlx(rename = "memory", try_from = "i64")]
    pub mem: u64,
    #[sqlx(try_from = "i64")]
    pub cpus: u64,
    #[sqlx(try_from = "i64")]
    pub gpus: u64,
}

impl Default for ResourceRequest {
    fn default() -> Self {
        Self {
            mem: 2048,
            cpus: 2,
            gpus: 0,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, sqlx::FromRow)]
pub struct Program {
    #[serde(deserialize_with = "deserialize_hash_from_json")]
    pub hash: Hash,
    pub name: String,
    pub image_file_name: String,
    pub image_file_url: String,
    pub image_file_checksum: String,
    #[sqlx(skip)]
    pub limits: Option<ResourceRequest>,
}

impl From<transaction::ProgramMetadata> for Program {
    fn from(value: transaction::ProgramMetadata) -> Self {
        Program {
            hash: value.hash,
            name: value.name,
            image_file_name: value.image_file_name,
            image_file_url: value.image_file_url,
            image_file_checksum: value.image_file_checksum,
            limits: value.resource_requirements,
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
            resource_requirements: value.limits,
        }
    }
}
