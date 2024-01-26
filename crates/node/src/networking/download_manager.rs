use crate::cli::Config;
use eyre::eyre;
use eyre::Result;
use futures_util::TryStreamExt;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full, StreamBody};
use hyper::body::{self, Bytes, Frame};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use std::net::SocketAddr;
use std::path::Path;
use tokio::fs::File;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio_util::io::ReaderStream;

/// download downloads file from the given `url` and saves it to file in `file_path`.
pub async fn download_file(
    url: &str,
    local_directory_path: &Path,
    file: &str,
    http_peer_list: Vec<(SocketAddr, Option<u16>)>,
    http_client: &reqwest::Client,
    file_hash: gevulot_node::types::Hash,
) -> Result<()> {
    tracing::trace!("download_file url:{url} local_directory_path:{local_directory_path:?} file:{file} file_hash:{file_hash} http_peer_list:{http_peer_list:?}");
    let url = reqwest::Url::parse(url)?;
    let mut resp = match http_client.get(url.clone()).send().await {
        Ok(resp) => resp,
        Err(err) => {
            let peer_urls: Vec<reqwest::Url> = http_peer_list
                .iter()
                .filter_map(|(peer, port)| {
                    port.map(|port| {
                        //use parse to create an URL, no new method.
                        let mut url = reqwest::Url::parse("http://localhost").unwrap(); //unwrap always succeed
                        url.set_ip_host(peer.ip()).unwrap(); //unwrap always succeed
                        url.set_port(Some(port)).unwrap(); //unwrap always succeed
                        url.set_path(file); //unwrap always succeed
                        url
                    })
                })
                .collect();
            let mut resp = None;
            for url in peer_urls {
                if let Ok(val) = http_client.get(url.clone()).send().await {
                    resp = Some(val);
                    break;
                }
            }
            match resp {
                Some(resp) => resp,
                _ => {
                    return Err(eyre!(
                        "Download no host found to download the file: {:?}",
                        file
                    ));
                }
            }
        }
    };

    if resp.status() == reqwest::StatusCode::OK {
        let file_path = local_directory_path.join(file);
        // Ensure any necessary subdirectories exists.
        if let Some(parent) = file_path.parent() {
            if let Ok(false) = tokio::fs::try_exists(parent).await {
                tokio::fs::create_dir_all(parent).await?;
            }
        }
        //create a tmp file during download.
        //this way the file won't be available for download from the other nodes
        //until it is completely written.
        let mut tmp_file_path = file_path.clone();
        tmp_file_path.set_extension("tmp");
        let fd = tokio::fs::File::create(&tmp_file_path).await?;
        let mut fd = tokio::io::BufWriter::new(fd);

        //create the Hasher to verify the Hash
        let mut hasher = blake3::Hasher::new();

        while let Some(chunk) = resp.chunk().await? {
            hasher.update(&chunk);
            fd.write_all(&chunk).await?;
        }

        fd.flush().await?;
        let checksum: gevulot_node::types::Hash = (&hasher.finalize()).into();
        if checksum != file_hash {
            Err(eyre!("Download file: {:?}, bad checksum", file))
        } else {
            //rename to original name
            Ok(std::fs::rename(tmp_file_path, file_path)?)
        }
    } else {
        Err(eyre!(
            "failed to download file from {}: response status: {}",
            url,
            resp.status()
        ))
    }
}

//start the local server and serve the specified file path.
//Return the server task join handle.
pub async fn serve_files(config: &Config) -> Result<JoinHandle<()>> {
    let mut bind_addr = config.p2p_listen_addr;
    bind_addr.set_port(config.http_download_port);
    let listener = TcpListener::bind(bind_addr).await?;

    let jh = tokio::spawn({
        let data_directory = config.data_directory.clone();
        async move {
            tracing::info!(
                "listening for http at {}",
                listener
                    .local_addr()
                    .expect("http listener's local address")
            );

            loop {
                match listener.accept().await {
                    Ok((stream, _from)) => {
                        let io = TokioIo::new(stream);
                        tokio::task::spawn({
                            let data_directory = data_directory.clone();
                            async move {
                                if let Err(err) = http1::Builder::new()
                                    .serve_connection(
                                        io,
                                        service_fn(|req| server_process_file(req, &data_directory)),
                                    )
                                    .await
                                {
                                    tracing::error!("Error serving node connection: {err}. Wait for a new node connection.");
                                }
                            }
                        });
                    }
                    Err(err) => {
                        tracing::error!("Error during node connection to file http server:{err}");
                    }
                }
            }
        }
    });

    Ok(jh)
}

async fn server_process_file(
    req: Request<body::Incoming>,
    data_directory: &Path,
) -> std::result::Result<Response<BoxBody<Bytes, std::io::Error>>, hyper::Error> {
    let file_digest = &req.uri().path()[1..];

    let mut file_path = data_directory.join(file_digest);

    let file = match File::open(&file_path).await {
        Ok(file) => file,
        Err(_) => {
            //try to see if the file is currently being updated.
            file_path.set_extension("tmp");
            let (status_code, message) = if file_path.as_path().exists() {
                (
                    StatusCode::PARTIAL_CONTENT,
                    "Update in progess, retry later",
                )
            } else {
                (StatusCode::NOT_FOUND, "File not found")
            };
            return Ok(Response::builder()
                .status(status_code)
                .body(Full::new(message.into()).map_err(|e| match e {}).boxed())
                .unwrap());
        }
    };

    let reader = ReaderStream::new(file);
    let stream_body = StreamBody::new(reader.map_ok(Frame::data));

    Ok(Response::builder()
        .status(StatusCode::OK)
        .body(BodyExt::boxed(stream_body))
        .unwrap())
}
