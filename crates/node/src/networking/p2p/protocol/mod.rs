use eyre::{eyre, Result};

pub mod internal;
pub mod v0;
pub mod v1;

pub fn serialize_handshake(msg: internal::Handshake) -> Result<Vec<u8>> {
    let msg = v0::Handshake::from(msg);
    bincode::serialize(&msg).map_err(|e| e.into())
}

pub fn new_serialize_handshake(protocol_version: u64, msg: internal::Handshake) -> Result<Vec<u8>> {
    match protocol_version {
        0 => bincode::serialize(&v0::Handshake::from(msg)),
        1 => bincode::serialize(&v1::Handshake::from(msg)),
        ver => return Err(eyre!("unknown protocol version: {ver}")),
    }
    .map(|mut bs| {
        let mut data = Vec::with_capacity(bs.len() + 8);
        data.append(&mut protocol_version.to_be_bytes().to_vec());
        data.append(&mut bs);
        data
    })
    .map_err(|e| e.into())
}

pub fn deserialize_handshake(bs: &[u8]) -> Result<internal::Handshake> {
    let res = v0::Handshake::parse(bs);
    if res.is_ok() {
        return res;
    }

    if bs.len() < 9 {
        return Err(eyre!("invalid handshake message: too short"));
    }

    match u64::from_be_bytes(bs[0..8].try_into().expect("convert slice to array")) {
        0 => v0::Handshake::parse(&bs[8..]),
        1 => v1::Handshake::parse(&bs[8..]),
        ver => Err(eyre!("unknown protocol version: {ver}")),
    }
}

pub fn serialize_msg(msg: internal::Message) -> Result<Vec<u8>> {
    let msg = v0::Message::from(msg);
    bincode::serialize(&msg).map_err(|e| e.into())
}

pub fn new_serialize_msg(protocol_version: u64, msg: internal::Message) -> Result<Vec<u8>> {
    match protocol_version {
        0 => bincode::serialize(&v0::Message::from(msg)),
        1 => bincode::serialize(&v1::Message::from(msg)),
        ver => return Err(eyre!("unknown protocol version: {ver}")),
    }
    .map(|mut bs| {
        let mut data = Vec::with_capacity(bs.len() + 8);
        data.append(&mut protocol_version.to_be_bytes().to_vec());
        data.append(&mut bs);
        data
    })
    .map_err(|e| e.into())
}

pub fn deserialize_msg(bs: &[u8]) -> Result<internal::Message> {
    let res = v0::Message::parse(bs);
    if res.is_ok() {
        return res;
    }

    if bs.len() < 9 {
        return Err(eyre!("invalid protocol message: too short"));
    }

    match u64::from_be_bytes(bs[0..8].try_into().expect("convert slice to array")) {
        0 => v0::Message::parse(&bs[8..]),
        1 => v1::Message::parse(&bs[8..]),
        ver => Err(eyre!("unknown protocol version: {ver}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use gevulot_node::types::{
        transaction::{Created, Payload, Validated},
        Transaction,
    };
    use libsecp256k1::SecretKey;
    use rand::{rngs::StdRng, SeedableRng};

    #[test]
    fn test_tx_serialization_between_v0_and_v1() {
        let orig_tx = new_tx();
        let bs =
            serialize_msg(internal::Message::Transaction(orig_tx.clone())).expect("serialize tx");

        let msg: v1::Message = bincode::deserialize(bs.as_ref()).expect("deserialize v1 message");
        if let v1::Message::V1(v1::MessageV1::Transaction(tx)) = msg {
            println!("deserialized tx: {:?}", tx);
            assert_eq!(orig_tx, tx);
        } else {
            panic!("test failed: Couldn't deserialize transaction correctly.");
        }
    }

    #[test]
    fn test_serialize_deserialize() {
        let orig_tx = new_tx();
        let bs =
            serialize_msg(internal::Message::Transaction(orig_tx.clone())).expect("serialize tx");
        let msg = deserialize_msg(bs.as_ref()).expect("deserialize_msg");

        if let internal::Message::Transaction(tx) = msg {
            println!("deserialized tx: {:?}", tx);
            assert_eq!(orig_tx, tx);
        } else {
            panic!("test failed: Couldn't deserialize transaction correctly.");
        }
    }

    #[test]
    fn test_v0_serialize_to_deserialize() {
        let orig_tx = new_tx();
        let msg = v0::Message::V0(v0::MessageV0::Transaction(orig_tx.clone()));
        let bs = bincode::serialize(&msg).expect("serialize message");

        let deserialized_msg = deserialize_msg(bs.as_ref()).expect("deserialize v0");

        if let internal::Message::Transaction(tx) = deserialized_msg {
            println!("deserialized tx: {:?}", &tx);
            assert_eq!(orig_tx, tx);
        } else {
            panic!("test failed: Couldn't deserialize transaction correctly.");
        }
    }

    #[test]
    fn test_new_serialize_v0_to_deserialize() {
        let orig_tx = new_tx();
        let msg = internal::Message::Transaction(orig_tx.clone());
        let bs = new_serialize_msg(0, msg).expect("serialize message");

        let deserialized_msg = deserialize_msg(bs.as_ref()).expect("deserialize v0");

        if let internal::Message::Transaction(tx) = deserialized_msg {
            println!("deserialized tx: {:?}", &tx);
            assert_eq!(orig_tx, tx);
        } else {
            panic!("test failed: Couldn't deserialize transaction correctly.");
        }
    }

    #[test]
    fn test_new_serialize_v1_to_deserialize() {
        let orig_tx = new_tx();
        let msg = internal::Message::Transaction(orig_tx.clone());
        let bs = new_serialize_msg(1, msg).expect("serialize message");

        let deserialized_msg = deserialize_msg(bs.as_ref()).expect("deserialize v0");

        if let internal::Message::Transaction(tx) = deserialized_msg {
            println!("deserialized tx: {:?}", &tx);
            assert_eq!(orig_tx, tx);
        } else {
            panic!("test failed: Couldn't deserialize transaction correctly.");
        }
    }

    fn new_tx() -> Transaction<Validated> {
        let rng = &mut StdRng::from_entropy();

        let tx = Transaction::<Created>::new(Payload::Empty, &SecretKey::random(rng));

        Transaction {
            author: tx.author,
            hash: tx.hash,
            payload: tx.payload,
            nonce: tx.nonce,
            signature: tx.signature,
            propagated: tx.executed,
            executed: tx.executed,
            state: Validated,
        }
    }
}
