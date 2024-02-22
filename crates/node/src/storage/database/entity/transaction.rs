use crate::storage::database::entity;
use crate::types::{self, transaction, Hash, Signature};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Deserialize, Serialize, sqlx::Type)]
#[sqlx(type_name = "transaction_kind", rename_all = "lowercase")]
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
    pub author: entity::PublicKey,
    pub hash: Hash,
    pub kind: Kind,
    pub nonce: sqlx::types::Decimal,
    pub signature: Signature,
    pub propagated: bool,
    pub executed: bool,
}

impl From<&types::Transaction<transaction::Validated>> for Transaction {
    fn from(value: &types::Transaction<transaction::Validated>) -> Self {
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
            author: entity::PublicKey(value.author),
            hash: value.hash,
            kind,
            nonce: value.nonce.into(),
            signature: value.signature,
            propagated: value.propagated,
            executed: value.executed,
        }
    }
}

impl From<Transaction> for types::Transaction<transaction::Validated> {
    fn from(value: Transaction) -> types::Transaction<transaction::Validated> {
        types::Transaction {
            author: value.author.into(),
            hash: value.hash,
            // This field is complemented separately.
            payload: transaction::Payload::Empty,
            nonce: TryFrom::<sqlx::types::Decimal>::try_from(value.nonce)
                .expect("invalid nonce in db"),
            signature: value.signature,
            propagated: value.propagated,
            executed: value.executed,
            state: transaction::Validated,
        }
    }
}
