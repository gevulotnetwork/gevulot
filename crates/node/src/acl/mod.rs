use async_trait::async_trait;
use eyre::Result;
use libsecp256k1::PublicKey;
use thiserror::Error;

#[allow(clippy::enum_variant_names)]
#[derive(Error, Debug)]
pub enum AclWhiteListError {
    #[error("An error occurs during ACLlist validation: {0}")]
    InternalError(String),
}

#[async_trait]
pub trait AclWhitelist: Send + Sync {
    async fn contains(&self, key: &PublicKey) -> Result<bool, AclWhiteListError>;
}

pub struct AlwaysGrantAclWhitelist;
#[async_trait::async_trait]
impl AclWhitelist for AlwaysGrantAclWhitelist {
    async fn contains(&self, _key: &PublicKey) -> Result<bool, AclWhiteListError> {
        Ok(true)
    }
}
