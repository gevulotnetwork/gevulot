use libsecp256k1::PublicKey;
use num_bigint::BigInt;

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct Account {
    public_key: PublicKey,
    balance: BigInt,
}
