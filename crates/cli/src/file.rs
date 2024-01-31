use crate::server;
use gevulot_node::types::transaction::ProgramMetadata;
use gevulot_node::types::Hash;
use sha3::Digest;
use sha3::Sha3_256;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use tokio::task::JoinHandle;

pub struct FileData {
    pub filename: String,
    pub url: String,
    pub checksum: Hash,
}

impl From<(String, String, Hash)> for FileData {
    fn from(data: (String, String, Hash)) -> Self {
        FileData {
            filename: data.0,
            url: data.1,
            checksum: data.2,
        }
    }
}

impl From<FileData> for ProgramMetadata {
    fn from(data: FileData) -> Self {
        let mut progdata = ProgramMetadata {
            name: data.filename.clone(),
            hash: Hash::default(),
            image_file_name: data.filename,
            image_file_url: data.url,
            image_file_checksum: data.checksum.to_string(),
        };
        progdata.update_hash();
        progdata
    }
}

pub async fn prepare_files_download<I>(
    bind_addr: Option<SocketAddr>,
    //input parameters file and file_url
    files_data_iter: I,
) -> crate::BoxResult<(Vec<FileData>, Option<JoinHandle<()>>)>
where
    I: Iterator<Item = (String, Option<String>)>,
{
    let mut local_server = None;
    let mut listen_addr = None;
    let mut server_data = HashMap::new();
    let mut file_data = vec![];

    for (file_or_hash, url) in files_data_iter {
        match verify_file_path(&file_or_hash) {
            Some(path) => {
                if local_server.is_none() {
                    let server = server::start_server(
                        bind_addr.unwrap_or(crate::HTTP_DEFAULT_ADDR.parse().unwrap()),
                    )
                    .await?;
                    // Re-read the listen address in case random port was used.
                    listen_addr = Some(server.local_addr()?);
                    local_server = Some(server);
                }

                let file = path.to_string_lossy();
                let (url, hash) = calculate_file_url_digest(&file, listen_addr.unwrap()); //unwrap tested just before.

                server_data.insert(hash, path.clone());
                let (filename, checksum) = extract_hash_from_file_content(&path)
                    .and_then(|file_hash| {
                        path.file_name()
                            .and_then(|file_name| file_name.to_str())
                            .map(|file_name| (file_name.to_string(), file_hash))
                    })
                    .ok_or(format!("Can't extract hash from file path:{path:?}"))?;

                file_data.push((filename, url, checksum).into());
            }
            None => {
                //verify the hash format and convert to hash.
                let hash = hex::decode(&file_or_hash)
                    .map_err(|err| format!("error during Hash decoding:{err}"))
                    .and_then(|val| {
                        let bytes = <[u8; 32]>::try_from(val).map_err(|err| {
                            format!("error Hash binary vec into array conv:{err:?}")
                        })?;
                        Ok(Hash::new(bytes))
                    })?;
                let file_url =
                    url.ok_or(format!("File url not provided with Hash:{file_or_hash}"))?;
                let filename = url::Url::parse(&file_url)
                    .map_err(|err| format!("File url:{file_url} parse error:{err}"))
                    .map(|url| {
                        url.path_segments()
                            .and_then(|iter| iter.last().map(String::from))
                            .unwrap_or(file_or_hash)
                    })?;
                file_data.push((filename, file_url, hash).into());
            }
        }
    }

    let jh = if let Some(listener) = local_server {
        let jh = server::serve_file(listener, server_data).await?;
        Some(jh)
    } else {
        None
    };

    Ok((file_data, jh))
}

pub fn extract_hash_from_file_content(path: &PathBuf) -> Option<Hash> {
    let mut hasher = blake3::Hasher::new();
    let fd = std::fs::File::open(path).ok()?;
    hasher.update_reader(fd).ok()?;
    let checksum = hasher.finalize();
    Some((&checksum).into())
}

//calculate the file name digest and the local server url.
//return the file path, file_url and file digest
fn calculate_file_url_digest(file_path: &str, listen_addr: SocketAddr) -> (String, String) {
    let mut hasher = Sha3_256::new();
    hasher.update(file_path.as_bytes());
    let digest = hex::encode(hasher.finalize());
    let file_url = format!("http://{}/{}", listen_addr, digest);
    (file_url, digest)
}

// Verify that the path exists and is a file.
fn verify_file_path(file_path: &str) -> Option<PathBuf> {
    let path: PathBuf = file_path.into();
    path.try_exists()
        .map(|res| res && path.is_file())
        .ok()
        .and_then(|present| present.then_some(path))
}
