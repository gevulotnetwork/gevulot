use std::fmt;

use serde::{Deserialize, Serialize};
use sqlx::{Decode, Encode, Postgres, Type};
use thiserror::Error;

#[allow(clippy::enum_variant_names)]
#[derive(Error, Debug)]
pub enum PublicKeyError {
    #[error("public key parse error: {0}")]
    ParseError(String),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PublicKey(pub libsecp256k1::PublicKey);

impl Default for PublicKey {
    fn default() -> Self {
        PublicKey(libsecp256k1::PublicKey::from_secret_key(
            &libsecp256k1::SecretKey::default(),
        ))
    }
}

impl fmt::Display for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", hex::encode(self.0.serialize()))
    }
}
impl From<PublicKey> for libsecp256k1::PublicKey {
    fn from(value: PublicKey) -> Self {
        value.0
    }
}

impl From<String> for PublicKey {
    fn from(value: String) -> Self {
        Self::try_from(value.as_str()).expect("from string")
    }
}

impl TryFrom<&str> for PublicKey {
    type Error = PublicKeyError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        let bs = hex::decode(value).map_err(|e| PublicKeyError::ParseError(e.to_string()))?;
        let bs: [u8; 65] = bs
            .try_into()
            .map_err(|_| PublicKeyError::ParseError("byte array length".to_string()))?;
        let pub_key = libsecp256k1::PublicKey::parse(&bs)
            .map_err(|e| PublicKeyError::ParseError(e.to_string()))?;
        Ok(PublicKey(pub_key))
    }
}

impl PublicKey {
    pub fn from_secret_key(secret_key: &libsecp256k1::SecretKey) -> PublicKey {
        PublicKey(libsecp256k1::PublicKey::from_secret_key(&secret_key))
    }
}

impl<'q> Decode<'q, Postgres> for PublicKey {
    fn decode(
        value: <Postgres as sqlx::database::HasValueRef<'q>>::ValueRef,
    ) -> Result<Self, sqlx::error::BoxDynError> {
        let str_value = <String as Decode<Postgres>>::decode(value)?;
        Ok(PublicKey::from(str_value))
    }
}

impl<'q> Encode<'q, Postgres> for PublicKey {
    fn encode_by_ref(
        &self,
        buf: &mut <Postgres as sqlx::database::HasArguments<'q>>::ArgumentBuffer,
    ) -> sqlx::encode::IsNull {
        let str_value = self.to_string();
        <String as Encode<Postgres>>::encode(str_value, buf)
    }
}

impl Type<Postgres> for PublicKey {
    fn type_info() -> <Postgres as sqlx::Database>::TypeInfo {
        <String as Type<Postgres>>::type_info()
    }

    fn compatible(ty: &<Postgres as sqlx::Database>::TypeInfo) -> bool {
        <String as Type<Postgres>>::compatible(ty)
    }
}
