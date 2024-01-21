use libsecp256k1::SecretKey;
use rand::rngs::StdRng;
use rand::SeedableRng;
use std::fs;
use std::path::PathBuf;

pub fn create_key_file(file_path: &PathBuf) -> crate::BoxResult<()> {
    let key = SecretKey::random(&mut StdRng::from_entropy());
    let key_array = key.serialize();
    if !file_path.as_path().exists() {
        Ok(fs::write(file_path, &key_array[..])?)
    } else {
        Err(Box::new(std::io::Error::new(
            std::io::ErrorKind::Other,
            "Key file already exist. Can't erase it.",
        )))
    }
}

pub fn read_key_file(file_path: &PathBuf) -> crate::BoxResult<SecretKey> {
    let key_array = fs::read(file_path)?;
    Ok(SecretKey::parse_slice(&key_array)?)
}
