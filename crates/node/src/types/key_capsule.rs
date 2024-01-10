use std::error::Error;

use libsecp256k1::{PublicKey, SecretKey};
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
pub struct KeyCapsule {
    pub msg: Vec<u8>,
    pub keys: Vec<(Vec<u8>, Vec<u8>)>,
}

impl KeyCapsule {
    pub fn new(msg: &[u8], recipients: &[PublicKey]) -> Self {
        // Generate fresh keypair for new capsule.
        let (sk, pk) = ecies::utils::generate_keypair();
        let (sk, pk) = (&sk.serialize(), &pk.serialize());

        // Encrypt the ephemeral key for each recipient.
        let mut keys: Vec<(Vec<u8>, Vec<u8>)> = vec![];
        for recipient in recipients {
            let key = ecies::encrypt(recipient.serialize().as_slice(), sk).unwrap();
            keys.push((recipient.serialize().to_vec(), key));
        }

        KeyCapsule {
            // Encrypt the message for ephemeral key.
            msg: ecies::encrypt(pk, msg).unwrap(),
            keys,
        }
    }

    pub fn decrypt(&self, secret_key: &SecretKey) -> Result<Vec<u8>, Box<dyn Error>> {
        let pk = &PublicKey::from_secret_key(secret_key).serialize();
        let sk = &secret_key.serialize();

        match self.keys.iter().find(|(recipient, _)| recipient == pk) {
            Some((_, key)) => {
                let key = ecies::decrypt(sk, key)?;
                let msg = ecies::decrypt(&key, self.msg.as_slice())?;
                Ok(msg.to_vec())
            }
            None => Err(String::from("recipient not present").into()),
        }
    }

    pub fn as_bytes(&self) -> Vec<u8> {
        bincode::serialize(self).unwrap()
    }
}

impl From<Vec<u8>> for KeyCapsule {
    fn from(value: Vec<u8>) -> Self {
        KeyCapsule::from(value.as_slice())
    }
}

impl From<&[u8]> for KeyCapsule {
    fn from(value: &[u8]) -> Self {
        bincode::deserialize(value).unwrap()
    }
}

#[cfg(test)]
mod tests {

    use rand::{rngs::StdRng, SeedableRng};

    use super::*;

    #[test]
    fn test_no_recipient() {
        let msg = "Hello, World!".as_bytes();
        let capsule = KeyCapsule::new(msg, &[]);

        assert_ne!(msg, capsule.msg);
        assert!(capsule.decrypt(&SecretKey::default()).is_err());
    }

    #[test]
    fn test_one_recipient() {
        let sk = SecretKey::random(&mut StdRng::from_entropy());
        let pk = PublicKey::from_secret_key(&sk);
        let msg = "Hello, World!".as_bytes();
        let capsule = KeyCapsule::new(msg, &[pk]);

        assert_ne!(msg, capsule.msg);
        assert_eq!(msg, capsule.decrypt(&sk).unwrap());
    }

    #[test]
    fn test_two_recipients() {
        let sk1 = SecretKey::random(&mut StdRng::from_entropy());
        let pk1 = PublicKey::from_secret_key(&sk1);
        let sk2 = SecretKey::random(&mut StdRng::from_entropy());
        let pk2 = PublicKey::from_secret_key(&sk2);

        let msg = "Hello, World!".as_bytes();
        let capsule = KeyCapsule::new(msg, &[pk1, pk2]);

        assert_ne!(msg, capsule.msg);
        assert_eq!(msg, capsule.decrypt(&sk1).unwrap());
        assert_eq!(msg, capsule.decrypt(&sk2).unwrap());
    }

    #[test]
    fn test_not_in_recipients() {
        let sk1 = SecretKey::random(&mut StdRng::from_entropy());
        let pk1 = PublicKey::from_secret_key(&sk1);
        let sk2 = SecretKey::random(&mut StdRng::from_entropy());

        let msg = "Hello, World!".as_bytes();
        let capsule = KeyCapsule::new(msg, &[pk1]);

        assert_ne!(msg, capsule.msg);
        assert_eq!(msg, capsule.decrypt(&sk1).unwrap());
        assert!(capsule.decrypt(&sk2).is_err());
    }

    #[test]
    fn test_serialization() {
        let sk1 = SecretKey::random(&mut StdRng::from_entropy());
        let pk1 = PublicKey::from_secret_key(&sk1);
        let sk2 = SecretKey::random(&mut StdRng::from_entropy());
        let pk2 = PublicKey::from_secret_key(&sk2);

        let msg = "Hello, World!".as_bytes();
        let capsule = KeyCapsule::new(msg, &[pk1, pk2]);

        let bs = capsule.as_bytes();
        let kc = KeyCapsule::from(bs);

        assert_eq!(capsule.msg, kc.msg);
        assert_eq!(capsule.keys, kc.keys);
    }
}
