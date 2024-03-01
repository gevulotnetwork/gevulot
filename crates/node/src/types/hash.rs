use libsecp256k1::Message;
use rand::Rng;
use serde::{de, Deserialize, Serialize};
use sqlx::{Decode, Encode, Postgres, Type};
use std::fmt;

pub const HASH_SIZE: usize = 32;

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct Hash([u8; HASH_SIZE]);

impl Hash {
    pub fn new(bytes: [u8; HASH_SIZE]) -> Self {
        Hash(bytes)
    }

    pub fn to_vec(&self) -> Vec<u8> {
        self.0.to_vec()
    }

    pub fn random<R: Rng>(rng: &mut R) -> Hash {
        let mut bs = [0u8; HASH_SIZE];
        rng.fill_bytes(&mut bs);
        Hash(bs)
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
        Self(value.try_into().expect("copy"))
    }
}

impl From<Hash> for Message {
    fn from(value: Hash) -> Self {
        // This function requires changes if HASH_SIZE changes. Add guard for it.
        const _: [(); 0 - { HASH_SIZE != 32 } as usize] = [];

        let x = &value.0;
        Message(libsecp256k1::curve::Scalar([
            u32::from_be_bytes([x[0], x[1], x[2], x[3]]),
            u32::from_be_bytes([x[4], x[5], x[6], x[7]]),
            u32::from_be_bytes([x[8], x[9], x[10], x[11]]),
            u32::from_be_bytes([x[12], x[13], x[14], x[15]]),
            u32::from_be_bytes([x[16], x[17], x[18], x[19]]),
            u32::from_be_bytes([x[20], x[21], x[22], x[23]]),
            u32::from_be_bytes([x[24], x[25], x[26], x[27]]),
            u32::from_be_bytes([x[28], x[29], x[30], x[31]]),
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
