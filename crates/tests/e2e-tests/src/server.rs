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
use std::path::PathBuf;
use std::sync::Arc;
use tokio::fs::File;
use tokio::net::TcpListener;
use tokio::sync::RwLock;
use tokio_util::io::ReaderStream;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Clone)]
pub struct FileServer {
    file_map: Arc<RwLock<HashMap<String, String>>>,
    listen_addr: SocketAddr,
}

impl FileServer {
    pub async fn new(listen_addr: SocketAddr) -> Self {
        let mut server = FileServer {
            file_map: Arc::new(RwLock::new(HashMap::new())),
            listen_addr,
        };

        let addr = server.listen(listen_addr).await;
        server.listen_addr = addr.expect("FileServer::listen");
        server
    }

    pub async fn register_file(&self, file: &PathBuf) -> String {
        let file_name = file.to_string_lossy().to_string();
        let mut file_map = self.file_map.write().await;
        let mut hasher = Sha3_256::new();
        hasher.update(file_name.as_bytes());
        let digest = hex::encode(hasher.finalize());
        let url = format!("http://{}/{}", self.listen_addr.to_string(), digest);
        file_map.insert(digest, file_name);
        url
    }

    async fn listen(&self, listen_addr: SocketAddr) -> Result<SocketAddr> {
        let listener = TcpListener::bind(listen_addr).await?;

        // Re-read the listen address in case random port was used.
        let addr = listener.local_addr()?;

        tokio::spawn({
            let server = self.clone();
            async move {
                loop {
                    let (stream, _) = listener.accept().await.expect("accept");
                    let io = TokioIo::new(stream);

                    tokio::task::spawn({
                        let server = server.clone();
                        async move {
                            if let Err(err) = http1::Builder::new()
                                .serve_connection(io, service_fn(|req| server.serve_file(req)))
                                .await
                            {
                                println!("Error serving connection: {:?}", err);
                            }
                        }
                    });
                }
            }
        });

        println!("Listening on http://{}", addr);
        Ok(addr)
    }

    async fn serve_file(
        &self,
        req: Request<body::Incoming>,
    ) -> std::result::Result<Response<BoxBody<Bytes, std::io::Error>>, hyper::Error> {
        let path = &req.uri().path()[1..];

        // Acquire read lock.
        let file_map = self.file_map.read().await;

        println!("request file: {}", path);

        let file_path = match file_map.get(path) {
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
}
