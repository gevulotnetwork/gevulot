use serde::{de, Deserialize, Serialize};
use sqlx::{self, Decode, Encode, Postgres, Type};
use std::fmt;
use thiserror::Error;

#[allow(clippy::enum_variant_names)]
#[derive(Error, Debug)]
pub enum SignatureError {
    #[error("signature parse error: {0}")]
    ParseError(String),
}

/// Scalar is mirror of libsecp256k1::curve::Scalar; replicated for control
/// over its interface.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Scalar(pub [u32; 8]);

impl From<libsecp256k1::curve::Scalar> for Scalar {
    fn from(value: libsecp256k1::curve::Scalar) -> Self {
        Self(value.0)
    }
}

impl From<Scalar> for libsecp256k1::curve::Scalar {
    fn from(value: Scalar) -> Self {
        libsecp256k1::curve::Scalar(value.0)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Signature {
    pub r: Scalar,
    pub s: Scalar,
}

impl fmt::Display for Signature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}",
            hex::encode(Into::<libsecp256k1::Signature>::into(*self).serialize())
        )
    }
}

impl From<String> for Signature {
    fn from(value: String) -> Self {
        Self::try_from(value.as_str()).expect("from string")
    }
}

impl TryFrom<&str> for Signature {
    type Error = SignatureError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        let bs = hex::decode(value).map_err(|e| SignatureError::ParseError(e.to_string()))?;
        let bs: [u8; 64] = bs
            .try_into()
            .map_err(|_| SignatureError::ParseError("byte array length".to_string()))?;
        let sig = libsecp256k1::Signature::parse_standard(&bs)
            .map_err(|e| SignatureError::ParseError(e.to_string()))?;
        Ok(From::<libsecp256k1::Signature>::from(sig))
    }
}

impl From<libsecp256k1::Signature> for Signature {
    fn from(value: libsecp256k1::Signature) -> Self {
        Signature {
            r: value.r.into(),
            s: value.s.into(),
        }
    }
}

impl From<Signature> for libsecp256k1::Signature {
    fn from(value: Signature) -> Self {
        libsecp256k1::Signature {
            r: value.r.into(),
            s: value.s.into(),
        }
    }
}

impl<'q> Decode<'q, Postgres> for Signature {
    fn decode(
        value: <Postgres as sqlx::database::HasValueRef<'q>>::ValueRef,
    ) -> Result<Self, sqlx::error::BoxDynError> {
        let str_value = <String as Decode<Postgres>>::decode(value)?;
        Ok(Signature::from(str_value))
    }
}

impl<'q> Encode<'q, Postgres> for Signature {
    fn encode_by_ref(
        &self,
        buf: &mut <Postgres as sqlx::database::HasArguments<'q>>::ArgumentBuffer,
    ) -> sqlx::encode::IsNull {
        let str_value = self.to_string();
        <String as Encode<Postgres>>::encode(str_value, buf)
    }
}

impl Type<Postgres> for Signature {
    fn type_info() -> <Postgres as sqlx::Database>::TypeInfo {
        <String as Type<Postgres>>::type_info()
    }

    fn compatible(ty: &<Postgres as sqlx::Database>::TypeInfo) -> bool {
        <String as Type<Postgres>>::compatible(ty)
    }
}

#[allow(dead_code)]
pub fn deserialize_hash_from_json<'de, D>(deserializer: D) -> Result<Signature, D::Error>
where
    D: de::Deserializer<'de>,
{
    struct JsonStringVisitor;

    impl<'de> de::Visitor<'de> for JsonStringVisitor {
        type Value = Signature;

        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            formatter.write_str("a hex encoded Signature")
        }

        fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Signature::try_from(v).map_err(E::custom)
        }
    }

    deserializer.deserialize_any(JsonStringVisitor)
}
