pub mod p2p;

use async_trait::async_trait;
use bytes::Buf;
use eyre::{eyre, Result};
use futures_util::TryStreamExt;
use std::collections::HashSet;
use std::sync::Arc;
use tokio::io::AsyncBufReadExt;
use tokio_stream::Stream;
use tokio_util::io::StreamReader;

pub use p2p::P2P;

use crate::storage::Database;

#[async_trait]
trait MergeStoredAcl {
    async fn get_acl_whitelist(&self) -> Result<Vec<crate::entity::PublicKey>>;
    async fn acl_whitelist(&self, key: crate::entity::PublicKey) -> Result<()>;
    async fn acl_deny(&self, key: &crate::entity::PublicKey) -> Result<()>;
}

pub struct WhitelistSyncer {
    url: String,
    database: Arc<Database>,
}

impl WhitelistSyncer {
    pub fn new(url: String, database: Arc<Database>) -> Self {
        Self { url, database }
    }

    pub async fn sync(&self) -> Result<()> {
        let url = reqwest::Url::parse(&self.url)?;
        let client = reqwest::ClientBuilder::new().gzip(true).build()?;

        let resp = client.get(url.clone()).send().await?;

        if resp.status() == reqwest::StatusCode::OK {
            let reader = StreamReader::new(
                resp.bytes_stream()
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e)),
            );
            merge_acl_white_list(reader, &self).await
        } else {
            Err(eyre!(
                "failed to download file from {}: response status: {}",
                url,
                resp.status()
            ))
        }
    }
}

#[async_trait::async_trait]
impl<'a> MergeStoredAcl for &'a WhitelistSyncer {
    async fn get_acl_whitelist(&self) -> Result<Vec<crate::entity::PublicKey>> {
        self.database.get_acl_whitelist().await
    }
    async fn acl_whitelist(&self, key: crate::entity::PublicKey) -> Result<()> {
        self.database.acl_whitelist(key).await
    }
    async fn acl_deny(&self, key: &crate::entity::PublicKey) -> Result<()> {
        self.database.acl_deny(key).await
    }
}

async fn merge_acl_white_list<S, B>(
    reader: StreamReader<S, B>,
    database: &impl MergeStoredAcl,
) -> Result<()>
where
    S: Stream<Item = Result<B, std::io::Error>> + Unpin,
    B: Buf,
{
    let mut lines = reader.lines();
    let db_keys = database.get_acl_whitelist().await?;
    let mut db_keys: HashSet<crate::entity::PublicKey> = HashSet::from_iter(db_keys.into_iter());

    let mut new_keys = vec![];
    // Received list shouldn't be empty.
    let mut non_empty_list = false;
    // If an error occurs during fetch cancel merge to avoid removing all db keys.
    while let Some(line) = lines.next_line().await? {
        non_empty_list = true;
        match crate::entity::PublicKey::try_from(line.as_str()) {
            Ok(public_key) => {
                if !db_keys.contains(&public_key) {
                    new_keys.push(public_key);
                } else {
                    db_keys.remove(&public_key);
                }
            }
            Err(err) => tracing::error!(
                "Error during acl white list sync. Key not a public key:{}",
                line
            ),
        }
    }

    let key_count = new_keys.len();
    for public_key in new_keys {
        database.acl_whitelist(public_key).await?;
    }
    let removed_key_count = db_keys.len();

    // Do not remove all list if received list is empty.
    if non_empty_list {
        for public_key in db_keys {
            database.acl_deny(&public_key).await?;
        }
    }

    tracing::info!("{key_count} new keys whitelisted and {removed_key_count} keys removed",);
    Ok(())
}

#[cfg(test)]
mod tests {

    use super::*;
    use tokio::sync::Mutex;

    struct TestDb(Mutex<HashSet<String>>);

    #[async_trait::async_trait]
    impl MergeStoredAcl for TestDb {
        async fn get_acl_whitelist(&self) -> Result<Vec<crate::entity::PublicKey>> {
            self.0
                .lock()
                .await
                .iter()
                .map(|s| crate::entity::PublicKey::try_from(s.as_str()))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|err| err.into())
        }
        async fn acl_whitelist(&self, key: crate::entity::PublicKey) -> Result<()> {
            self.0.lock().await.insert(key.to_string());
            Ok(())
        }
        async fn acl_deny(&self, key: &crate::entity::PublicKey) -> Result<()> {
            let str_key = key.to_string();
            self.0.lock().await.remove(&str_key);
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_acl_merge_multiple() {
        let db = TestDb(Mutex::new(HashSet::new()));
        {
            let mut set = db.0.lock().await;
            set.insert("0479be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798483ada7726a3c4655da4fbfc0e1108a8fd17b448a68554199c47d08ffb10d4b8".to_string());
            set.insert("040f28123df7a638647d867dfe186395999a276048c76c1bd56d4b792eca56449751944d6cd0b208f44b9aec332b4b6d0630406ebb07ccfb17e7c505a07156a792".to_string());
            set.insert("04485539698460eb21864f22fdc2ff595980f67d6b43c90b35e022ea40a7465806144bdfe79e571ee196ce52f7d0c88da915733e838420b347f8e6f7920c2c91e7".to_string());
        }

        let to_merge_acl = vec![
            Ok("043a1d3d3bfa8b91b18be20009f58616683695765c90c37a404af7635a3e047de50ee62b5da0c02a1ba54489294974aa6a3733ec82e34c8f6a26c29d6aa000e88d\n".as_bytes()),
            Ok("040f28123df7a638647d867dfe186395999a276048c76c1bd56d4b792eca56449751944d6cd0b208f44b9aec332b4b6d0630406ebb07ccfb17e7c505a07156a792\n".as_bytes()),
        ];
        let reader = StreamReader::new(tokio_stream::iter(to_merge_acl));
        let res = merge_acl_white_list(reader, &db).await;

        assert!(res.is_ok());
        {
            let set = db.0.lock().await;
            assert_eq!(2, set.len());
            assert!(set.get("043a1d3d3bfa8b91b18be20009f58616683695765c90c37a404af7635a3e047de50ee62b5da0c02a1ba54489294974aa6a3733ec82e34c8f6a26c29d6aa000e88d").is_some());
            assert!(set.get("040f28123df7a638647d867dfe186395999a276048c76c1bd56d4b792eca56449751944d6cd0b208f44b9aec332b4b6d0630406ebb07ccfb17e7c505a07156a792").is_some());
            assert!(set.get("0479be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798483ada7726a3c4655da4fbfc0e1108a8fd17b448a68554199c47d08ffb10d4b8").is_none());
            assert!(set.get("04485539698460eb21864f22fdc2ff595980f67d6b43c90b35e022ea40a7465806144bdfe79e571ee196ce52f7d0c88da915733e838420b347f8e6f7920c2c91e7").is_none());
        }
    }

    #[tokio::test]
    async fn test_acl_merge_from_empty() {
        let db = TestDb(Mutex::new(HashSet::new()));

        let to_merge_acl = vec![
            Ok("043a1d3d3bfa8b91b18be20009f58616683695765c90c37a404af7635a3e047de50ee62b5da0c02a1ba54489294974aa6a3733ec82e34c8f6a26c29d6aa000e88d\n".as_bytes()),
            Ok("040f28123df7a638647d867dfe186395999a276048c76c1bd56d4b792eca56449751944d6cd0b208f44b9aec332b4b6d0630406ebb07ccfb17e7c505a07156a792\n".as_bytes()),
        ];
        let reader = StreamReader::new(tokio_stream::iter(to_merge_acl));
        let res = merge_acl_white_list(reader, &db).await;

        assert!(res.is_ok());
        {
            let set = db.0.lock().await;
            assert_eq!(2, set.len());
            assert!(set.get("043a1d3d3bfa8b91b18be20009f58616683695765c90c37a404af7635a3e047de50ee62b5da0c02a1ba54489294974aa6a3733ec82e34c8f6a26c29d6aa000e88d").is_some());
            assert!(set.get("040f28123df7a638647d867dfe186395999a276048c76c1bd56d4b792eca56449751944d6cd0b208f44b9aec332b4b6d0630406ebb07ccfb17e7c505a07156a792").is_some());
        }
    }

    #[tokio::test]
    async fn test_acl_merge_with_empty() {
        let db = TestDb(Mutex::new(HashSet::new()));
        {
            let mut set = db.0.lock().await;
            set.insert("0479be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798483ada7726a3c4655da4fbfc0e1108a8fd17b448a68554199c47d08ffb10d4b8".to_string());
        }

        let to_merge_acl: Vec<Result<&[u8], std::io::Error>> = vec![];
        let reader = StreamReader::new(tokio_stream::iter(to_merge_acl));
        let res = merge_acl_white_list(reader, &db).await;

        assert!(res.is_ok());
        {
            let set = db.0.lock().await;
            assert_eq!(1, set.len());
            assert!(set.get("0479be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798483ada7726a3c4655da4fbfc0e1108a8fd17b448a68554199c47d08ffb10d4b8").is_some());
        }
    }

    #[tokio::test]
    async fn test_acl_merge_with_error() {
        let db = TestDb(Mutex::new(HashSet::new()));
        {
            let mut set = db.0.lock().await;
            set.insert("0479be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798483ada7726a3c4655da4fbfc0e1108a8fd17b448a68554199c47d08ffb10d4b8".to_string());
        }

        let to_merge_acl = vec![
            Ok("043a1d3d3bfa8b91b18be20009f58616683695765c90c37a404af7635a3e047de50ee62b5da0c02a1ba54489294974aa6a3733ec82e34c8f6a26c29d6aa000e88d\n".as_bytes()),
            Err(std::io::Error::new(std::io::ErrorKind::Other, "a test")), 
            Ok("040f28123df7a638647d867dfe186395999a276048c76c1bd56d4b792eca56449751944d6cd0b208f44b9aec332b4b6d0630406ebb07ccfb17e7c505a07156a792\n".as_bytes()),
        ];
        let reader = StreamReader::new(tokio_stream::iter(to_merge_acl));
        let res = merge_acl_white_list(reader, &db).await;

        assert!(res.is_err());
        {
            let set = db.0.lock().await;
            assert_eq!(1, set.len());
            assert!(set.get("0479be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798483ada7726a3c4655da4fbfc0e1108a8fd17b448a68554199c47d08ffb10d4b8").is_some());
        }
    }

    #[tokio::test]
    async fn test_acl_merge_with_keyparseerror() {
        let db = TestDb(Mutex::new(HashSet::new()));
        {
            let mut set = db.0.lock().await;
            set.insert("0479be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798483ada7726a3c4655da4fbfc0e1108a8fd17b448a68554199c47d08ffb10d4b8".to_string());
        }

        let to_merge_acl = vec![
            Ok("043a1d3d3bfa8b91b18be20009f58616683695765c90c37a404af7635a3e047de50ee62b5da0c02a1ba54489294974aa6a3733ec82e34c8f6a26c29d6aa000e88d\n".as_bytes()),
            Ok("0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000\n".as_bytes()),
            Ok("040f28123df7a638647d867dfe186395999a276048c76c1bd56d4b792eca56449751944d6cd0b208f44b9aec332b4b6d0630406ebb07ccfb17e7c505a07156a792\n".as_bytes()),
        ];
        let reader = StreamReader::new(tokio_stream::iter(to_merge_acl));
        let res = merge_acl_white_list(reader, &db).await;

        assert!(res.is_ok());
        {
            let set = db.0.lock().await;
            assert_eq!(2, set.len());
            assert!(set.get("043a1d3d3bfa8b91b18be20009f58616683695765c90c37a404af7635a3e047de50ee62b5da0c02a1ba54489294974aa6a3733ec82e34c8f6a26c29d6aa000e88d").is_some());
            assert!(set.get("040f28123df7a638647d867dfe186395999a276048c76c1bd56d4b792eca56449751944d6cd0b208f44b9aec332b4b6d0630406ebb07ccfb17e7c505a07156a792").is_some());
        }
    }
}
