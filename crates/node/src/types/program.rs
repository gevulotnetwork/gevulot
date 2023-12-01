use serde::{Deserialize, Serialize};

use super::hash::{deserialize_hash_from_json, Hash};

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
