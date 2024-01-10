use libsecp256k1::Message;
use rand::Rng;
use serde::{de, Deserialize, Serialize};
use sqlx::{self, Decode, Encode, Postgres, Type};
use std::fmt;

const HASH_SIZE: usize = 32;

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct Hash([u8; HASH_SIZE]);

impl Hash {
    pub fn to_vec(&self) -> Vec<u8> {
        self.0.to_vec()
    }

    pub fn random<R: Rng>(rng: &mut R) -> Hash {
        let mut bs = [0u8; HASH_SIZE];
        rng.fill_bytes(&mut bs);
        Hash { 0: bs }
    }
}

impl AsRef<Hash> for Hash {
    fn as_ref(&self) -> &Hash {
        self
    }
}

impl fmt::Display for Hash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", hex::encode(self))
    }
}

impl From<&blake3::Hash> for Hash {
    fn from(value: &blake3::Hash) -> Self {
        Hash(*value.as_bytes())
    }
}

impl From<&str> for Hash {
    fn from(value: &str) -> Self {
        Hash(
            hex::decode(value)
                .expect("invalid value")
                .try_into()
                .unwrap(),
        )
    }
}

impl From<String> for Hash {
    fn from(value: String) -> Self {
        Self::from(value.as_str())
    }
}

impl From<&[u8]> for Hash {
    fn from(value: &[u8]) -> Self {
        Self {
            0: value.try_into().expect("copy"),
        }
    }
}

impl From<Hash> for Message {
    fn from(value: Hash) -> Self {
        let chunk_sz = HASH_SIZE / 8;
        let mut offset = 0;
        let mut chunks = vec![];
        for _ in 0..value.0.len() / chunk_sz {
            chunks.push(
                <&[u8] as TryInto<[u8; 4]>>::try_into(&value.0[offset..offset + chunk_sz])
                    .unwrap()
                    .clone(),
            );
            offset += chunk_sz;
        }

        let mut chunks = chunks.iter_mut();
        Message(libsecp256k1::curve::Scalar([
            u32::from_be_bytes(*chunks.next().unwrap()),
            u32::from_be_bytes(*chunks.next().unwrap()),
            u32::from_be_bytes(*chunks.next().unwrap()),
            u32::from_be_bytes(*chunks.next().unwrap()),
            u32::from_be_bytes(*chunks.next().unwrap()),
            u32::from_be_bytes(*chunks.next().unwrap()),
            u32::from_be_bytes(*chunks.next().unwrap()),
            u32::from_be_bytes(*chunks.next().unwrap()),
        ]))
    }
}

impl AsRef<[u8]> for Hash {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl<'q> Decode<'q, Postgres> for Hash {
    fn decode(
        value: <Postgres as sqlx::database::HasValueRef<'q>>::ValueRef,
    ) -> Result<Self, sqlx::error::BoxDynError> {
        let str_value = <String as Decode<Postgres>>::decode(value)?;
        Ok(Hash::from(str_value))
    }
}

impl<'q> Encode<'q, Postgres> for Hash {
    fn encode_by_ref(
        &self,
        buf: &mut <Postgres as sqlx::database::HasArguments<'q>>::ArgumentBuffer,
    ) -> sqlx::encode::IsNull {
        let str_value = self.to_string();
        <String as Encode<Postgres>>::encode(str_value, buf)
    }
}

impl Type<Postgres> for Hash {
    fn type_info() -> <Postgres as sqlx::Database>::TypeInfo {
        <String as Type<Postgres>>::type_info()
    }

    fn compatible(ty: &<Postgres as sqlx::Database>::TypeInfo) -> bool {
        <String as Type<Postgres>>::compatible(ty)
    }
}

pub fn deserialize_hash_from_json<'de, D>(deserializer: D) -> Result<Hash, D::Error>
where
    D: de::Deserializer<'de>,
{
    struct JsonStringVisitor;

    impl<'de> de::Visitor<'de> for JsonStringVisitor {
        type Value = Hash;

        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            formatter.write_str("a hex encoded Hash")
        }

        fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Ok(Hash::from(v))
        }
    }

    deserializer.deserialize_any(JsonStringVisitor)
}
