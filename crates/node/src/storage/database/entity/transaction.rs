use crate::types::{self, transaction, Hash, Signature};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Deserialize, Serialize, sqlx::Type)]
pub enum Kind {
    #[default]
    Empty,
    Transfer,
    Stake,
    Unstake,
    Deploy,
    Run,
    Proof,
    ProofKey,
    Verification,
    Cancel,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, sqlx::FromRow)]
pub struct Transaction {
    pub hash: Hash,
    pub kind: Kind,
    pub nonce: sqlx::types::Decimal,
    pub signature: Signature,
    pub propagated: bool,
}

impl From<types::Transaction> for Transaction {
    fn from(value: types::Transaction) -> Self {
        let kind = match value.payload {
            transaction::Payload::Empty => Kind::Empty,
            transaction::Payload::Transfer { .. } => Kind::Transfer,
            transaction::Payload::Stake { .. } => Kind::Stake,
            transaction::Payload::Unstake { .. } => Kind::Unstake,
            transaction::Payload::Deploy { .. } => Kind::Deploy,
            transaction::Payload::Run { .. } => Kind::Run,
            transaction::Payload::Proof { .. } => Kind::Proof,
            transaction::Payload::ProofKey { .. } => Kind::ProofKey,
            transaction::Payload::Verification { .. } => Kind::Verification,
            transaction::Payload::Cancel { .. } => Kind::Cancel,
        };

        Transaction {
            hash: value.hash,
            kind,
            nonce: value.nonce.into(),
            signature: value.signature,
            propagated: value.propagated,
        }
    }
}

impl From<Transaction> for types::Transaction {
    fn from(value: Transaction) -> types::Transaction {
        types::Transaction {
            hash: value.hash,
            // This field is complemented separately.
            payload: transaction::Payload::Empty,
            nonce: TryFrom::<sqlx::types::Decimal>::try_from(value.nonce)
                .expect("invalid nonce in db"),
            signature: value.signature,
            propagated: value.propagated,
        }
    }
}
