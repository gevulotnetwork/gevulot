use std::{error::Error, fs::File, io::Write};

use libsecp256k1::SecretKey;
use rand::{rngs::StdRng, SeedableRng};

fn main() -> Result<(), Box<dyn Error>> {
    let key = SecretKey::random(&mut StdRng::from_entropy());
    let mut fd = File::create("/var/lib/gevulot/node.key")?;
    fd.write_all(&key.serialize()[..])?;
    fd.flush()?;

    Ok(())
}
