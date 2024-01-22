use crate::cli::Config;
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
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio_util::io::ReaderStream;

pub struct DownloadManager {
    file_server_jh: JoinHandle<()>,
}

impl DownloadManager {
    pub async fn new(config: &Config, bind_addr: SocketAddr) -> Result<Self> {
        let file_server_jh = serve_file(config, bind_addr).await?;
        Ok(DownloadManager { file_server_jh })
    }
}

//start the local server and serve the specified file path.
//Return the file_names and associated Url to get the file from the server.
pub async fn serve_file(config: &Config, bind_addr: SocketAddr) -> Result<JoinHandle<()>> {
    let listener = TcpListener::bind(bind_addr).await?;

    let jh = tokio::spawn({
        let data_directory = config.data_directory.clone();
        async move {
            loop {
                match listener.accept().await {
                    Ok((stream, _)) => {
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

    let mut file_path = data_directory.join("images").join(file_digest);

    let file = match File::open(&file_path).await {
        Ok(file) => file,
        Err(_) => {
            //try to see if the file is currently being updated.
            file_path.set_extension(".tmp");
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

    let file_hash = file_digest.to_string();
    let reader = ReaderStream::new(file);
    let stream_body = StreamBody::new(reader.map_ok(Frame::data));

    Ok(Response::builder()
        .status(StatusCode::OK)
        .body(BodyExt::boxed(stream_body))
        .unwrap())
}
