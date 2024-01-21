use futures_util::StreamExt;
use futures_util::TryStreamExt;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full, StreamBody};
use hyper::body::{self, Bytes, Frame};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use indicatif::{MultiProgress, ProgressBar, ProgressState, ProgressStyle};
use sha3::{Digest, Sha3_256};
use std::collections::HashMap;
use std::fmt::Write;
use std::net::SocketAddr;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::fs::File;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio::time::{self, Duration};
use tokio_util::io::ReaderStream;

//start the local server and serve the specified file path.
//Return the file_names and associated Url to get the file from the server.
pub async fn serve_file(
    bind_addr: SocketAddr,
    files: &[PathBuf],
) -> crate::BoxResult<(HashMap<&PathBuf, String>, JoinHandle<()>)> {
    let listener = TcpListener::bind(bind_addr).await?;

    // Re-read the listen address in case random port was used.
    let listener_addr = listener.local_addr()?;

    let files_data: Vec<(&PathBuf, (String, String))> = files
        .iter()
        .map(|path| (path, calculate_file_url_digest(path, listener_addr)))
        .collect();
    //create the list of file to be served.
    let mut served_files: HashMap<String, PathBuf> = files_data
        .iter()
        .map(|(path, (_url, hash))| (hash.to_string(), path.to_path_buf()))
        .collect();

    //build progress bar
    let multi_pg = MultiProgress::new();
    let pg_map: HashMap<String, ProgressBar> = served_files
        .iter()
        .map::<Result<_, std::io::Error>, _>(|(digest, path)| {
            let metadata = std::fs::metadata(path)?;
            let filename = path
                .file_name()
                .and_then(|file_name| file_name.to_str())
                .unwrap_or("img_file");
            let pg = multi_pg.add(build_file_progress_bar(
                filename.to_string(),
                metadata.len(),
            ));
            Ok((digest.to_string(), pg))
        })
        .filter_map(Result::ok)
        .collect();

    let jh = tokio::spawn({
        async move {
            let local_file_list = Arc::new(served_files.clone());
            let (counter_tx, mut counter_rx) = tokio::sync::mpsc::unbounded_channel();

            //timer that detect if the download has been started before it trigger.
            //node download should start before 10 second.
            let mut download_started = false;
            let mut download_started_interval = time::interval(Duration::from_millis(10000));
            download_started_interval.tick().await;

            loop {
                tokio::select! {
                    listen = listener.accept()  => match listen {
                        Ok((stream, _)) => {
                            let io = TokioIo::new(stream);
                            tokio::task::spawn({
                                let local_file_list = local_file_list.clone();
                                let conn_counter_tx = counter_tx.clone();
                                async move {
                                    if let Err(err) = http1::Builder::new()
                                        .serve_connection(
                                            io,
                                            service_fn(|req| server_process_file(req, &local_file_list, conn_counter_tx.clone())),
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
                    },
                    //manage file download tracking.
                    Some((file_digest, byte_len)) = counter_rx.recv() => {
                        download_started = true;
                        if let Some(pg) = pg_map.get(&file_digest) {
                            pg.inc(byte_len as u64);
                            if pg.length().unwrap() <= pg.position() { //unwrap because always present, inited in the pg constructor.
                                pg.finish_with_message("Uploaded");
                                served_files.remove(&file_digest);
                            }

                        }
                        if served_files.is_empty() {
                            //end of the download
                            //wait that the  http download buffer flush.
                            tokio::time::sleep(tokio::time::Duration::from_millis(250)).await;
                            break;
                        }
                    }
                    _ = download_started_interval.tick() => {
                        if !download_started {
                            log::error!("Node download didn't started in time. Local host can be unreachable from node.");
                            break;
                        }
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
    Ok((ret, jh))
}

async fn server_process_file(
    req: Request<body::Incoming>,
    files: &HashMap<String, PathBuf>,
    counter_sender: tokio::sync::mpsc::UnboundedSender<(String, usize)>,
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

    let file_hash = file_digest.to_string();
    let reader = ReaderStream::new(file).map(move |bytes| {
        let nb_bytes = bytes.as_ref().map(|b| b.len()).unwrap_or(0);
        if let Err(err) = counter_sender.send((file_hash.clone(), nb_bytes)) {
            log::error!("An error occurs during file download. NOtofication channel close:{err}");
        }
        bytes
    });
    let stream_body = StreamBody::new(reader.map_ok(Frame::data));

    Ok(Response::builder()
        .status(StatusCode::OK)
        .body(BodyExt::boxed(stream_body))
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

fn build_file_progress_bar(file_name: String, total_size: u64) -> ProgressBar {
    let pb = ProgressBar::new(total_size);
    pb.set_style(ProgressStyle::with_template("{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {bytes}/{total_bytes} ({eta})")
        .unwrap()
        .with_key("eta", move |state: &ProgressState, w: &mut dyn Write| write!(w, "{}-{:.1}s", file_name, state.eta().as_secs_f64()).unwrap())
        .progress_chars("#>-"));
    pb
}
