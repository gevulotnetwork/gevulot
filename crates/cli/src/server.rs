use futures_util::TryStreamExt;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full, StreamBody};
use hyper::body::{self, Bytes, Frame};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use sha3::{Digest, Sha3_256};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::fs::File;
use tokio::net::TcpListener;
use tokio_util::io::ReaderStream;

//start the local server and serve the specified file path.
//Return the file_names and associated Url to get the file from the server.
pub async fn serve_file(
    bind_addr: SocketAddr,
    files: &[PathBuf],
) -> crate::Result<HashMap<&PathBuf, String>> {
    let listener = TcpListener::bind(bind_addr).await?;

    // Re-read the listen address in case random port was used.
    let listener_addr = listener.local_addr()?;

    let files_data: Vec<(&PathBuf, (String, String))> = files
        .iter()
        .map(|path| (path, calculate_file_url_digest(path, listener_addr)))
        .collect();
    //create the list of file to be served.
    let served_files: HashMap<String, PathBuf> = files_data
        .iter()
        .map(|(path, (_url, hash))| (hash.to_string(), path.to_path_buf()))
        .collect();

    tokio::spawn({
        async move {
            let served_files = Arc::new(served_files);
            loop {
                match listener.accept().await {
                    Ok((stream, _)) => {
                        let io = TokioIo::new(stream);
                        tokio::task::spawn({
                            let served_files = served_files.clone();
                            async move {
                                if let Err(err) = http1::Builder::new()
                                    .serve_connection(
                                        io,
                                        service_fn(|req| server_process_file(req, &served_files)),
                                    )
                                    .await
                                {
                                    log::error!("Error serving node connection: {err}. Wait for a new node connection.");
                                }
                            }
                        });
                    }
                    Err(err) => {
                        log::error!(
                            "Error during node connection to local server:{err} \
                                The img file hasn't been delivered. Wait for a new node connection"
                        );
                    }
                }
            }
        }
    });

    //return served file url.
    let ret = files_data
        .into_iter()
        .map(|(path, (url, _hash))| (path, url))
        .collect();
    Ok(ret)
}

async fn server_process_file(
    req: Request<body::Incoming>,
    files: &HashMap<String, PathBuf>,
) -> std::result::Result<Response<BoxBody<Bytes, std::io::Error>>, hyper::Error> {
    let file_digest = &req.uri().path()[1..];
    let file_path = match files.get(file_digest) {
        Some(file_path) => file_path,
        None => {
            return Ok(Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(
                    Full::new("Not found.".into())
                        .map_err(|e| match e {})
                        .boxed(),
                )
                .unwrap())
        }
    };
    let file = match File::open(file_path).await {
        Ok(file) => file,
        Err(_) => {
            return Ok(Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(
                    Full::new("Internal server error".into())
                        .map_err(|e| match e {})
                        .boxed(),
                )
                .unwrap())
        }
    };

    let reader = ReaderStream::new(file);
    let stream_body = StreamBody::new(reader.map_ok(Frame::data));

    Ok(Response::builder()
        .status(StatusCode::OK)
        .body(stream_body.boxed())
        .unwrap())
}

//calculate the file name digest and the local server url.
//return the file path, file_url and file digest
fn calculate_file_url_digest(file_path: &Path, listen_addr: SocketAddr) -> (String, String) {
    //use to_string_lossy because path verification should have been done before.
    let file_name = file_path.to_string_lossy();
    let mut hasher = Sha3_256::new();
    hasher.update(file_name.as_bytes());
    let digest = hex::encode(hasher.finalize());
    let file_url = format!("http://{}/{}", listen_addr, digest);
    (file_url, digest)
}
