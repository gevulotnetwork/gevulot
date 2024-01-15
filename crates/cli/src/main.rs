use clap::Parser;
use clap::Subcommand;
use gevulot_node::{
    rpc_client::RpcClient,
    types::{
        transaction::{Payload, ProgramMetadata},
        Hash, Transaction,
    },
};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;

mod keyfile;
mod server;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Parser, Debug)]
#[clap(author = "Gevulot Team", version, about, long_about = None)]
pub struct ArgConfiguration {
    /// RPC url of the Gevulot node
    #[clap(
        short,
        long,
        default_value = "http://localhost:9944",
        value_name = "URL"
    )]
    jsonurl: String,
    /// Private key file path to sign Tx.
    #[clap(
        short,
        long,
        default_value = "localkey.pki",
        value_name = "KEY FILE PATH"
    )]
    keyfile: PathBuf,
    #[command(subcommand)]
    command: ConfCommands,
}

#[derive(Subcommand, Debug)]
enum ConfCommands {
    /// Deploy prover and verifier.
    #[command(arg_required_else_help = true)]
    GenerateKey,

    /// Deploy prover and verifier.
    #[command(arg_required_else_help = true)]
    Deploy {
        /// name of the deployment.
        #[clap(short, long, value_name = "DEPLOYMENT NAME")]
        name: String,
        /// file path containing the img of the prover to deploy or the hash  of the prover img (--proverimgurl is mandorty in this case). If the  file is not found the parameters is used as an hash.
        #[clap(short, long, value_name = "PROVER FILE or HASH")]
        prover: String,
        /// file path containing the img of the verifier to deployor the hash  of the verifier img (--verifierimgurl is mandorty in this case). If the  file is not found the parameters is used as an hash.
        #[clap(short, long, value_name = "VERIFIER FILE or HASH")]
        verifier: String,

        /// url to get the prover img. If provided the prover will use this url to get the prover img. If not the cli tool start a local HTTP server to server the file to the node.
        #[clap(long, value_name = "PROVER URL")]
        proverimgurl: Option<String>,
        /// url to get the verifier img. If provided the verifier will use this url to get the verifier img. If not the cli tool start a local HTTP server to server the file to the node.
        #[clap(long, value_name = "VERIFIER URL")]
        verifierimgurl: Option<String>,
        /// Address the local http server use to listen for node file download request.
        #[clap(
            short,
            long,
            default_value = "localhost",
            value_name = "LOCAL SERVER BIND ADDR"
        )]
        listen_addr: SocketAddr,
    },

    /// Execute the list of task in the order one after the other.
    #[command(arg_required_else_help = true)]
    Exec {
        /// array of Json task definition.
        #[clap(short, long, value_name = "TASK ARRAY")]
        tasks: Vec<String>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let args = ArgConfiguration::parse();

    let client = RpcClient::new(args.jsonurl);

    match args.command {
        ConfCommands::GenerateKey => match keyfile::create_key_file(&args.keyfile) {
            Ok(()) => println!(
                "Key generated and saved in file:{}",
                args.keyfile.to_str().unwrap_or("")
            ),
            Err(err) => println!("Error during key  file creation:{err}"),
        },
        ConfCommands::Deploy {
            name,
            prover,
            verifier,
            proverimgurl,
            verifierimgurl,
            listen_addr,
        } => {
            match run_deploy_command(
                client,
                args.keyfile,
                name,
                prover,
                verifier,
                proverimgurl,
                verifierimgurl,
                listen_addr,
            )
            .await
            {
                Ok(()) => println!("Prover / Verifier deployed correctly."),
                Err(err) => println!("An error occurs during Prover / Verifier deployement :{err}"),
            }
        }
        ConfCommands::Exec { tasks } => {
            println!("Exec {tasks:?}");
        }
    }
    Ok(())
}

async fn run_deploy_command(
    client: RpcClient,
    keyfile: PathBuf,
    name: String,
    prover: String,
    verifier: String,
    proverimgurl: Option<String>,
    verifierimgurl: Option<String>,
    listen_addr: SocketAddr,
) -> Result<()> {
    let key = match keyfile::read_key_file(&keyfile) {
        Ok(key) => key,
        Err(err) => {
            println!(
                "Error during key file:{} reading:{err}",
                keyfile.to_str().unwrap_or("")
            );
            return Err(err);
        }
    };
    let exist_prover_path = verify_file_path(&prover);
    let exist_verifier_path = verify_file_path(&verifier);

    //start the local server if needed. Local file provided
    let server_files_path: Vec<PathBuf> = exist_prover_path
        .iter()
        .chain(exist_verifier_path.iter())
        .cloned()
        .collect();
    let served_file_map = if !server_files_path.is_empty() {
        //some local files start the server.
        match server::serve_file(listen_addr, &server_files_path).await {
            Ok(files) => files,
            Err(err) => {
                log::error!(
                    "Couldn't bind to specified bind address:{} because {err}",
                    listen_addr
                );
                return Err(err);
            }
        }
    } else {
        HashMap::new()
    };

    //create tx data
    let Ok(prover_data) = create_tx_data(exist_prover_path, prover, proverimgurl, &served_file_map)
    else {
        //try to see if it's a file hash
        log::error!("Wrong specified prover image file path or Hash. File can't be read.");
        return Err(
            "Wrong specified prover image file path. File can't be read."
                .to_string()
                .into(),
        );
    };
    let Ok(verifier_data) = create_tx_data(
        exist_verifier_path,
        verifier,
        verifierimgurl,
        &served_file_map,
    ) else {
        //try to see if it's a file hash
        log::error!("Wrong specified verifier image file path or Hash. File can't be read.");
        return Err(
            "Wrong specified verifier image file path. File can't be read."
                .to_string()
                .into(),
        );
    };

    let mut tx = Transaction {
        payload: Payload::Deploy {
            name,
            prover: prover_data,
            verifier: verifier_data,
        },
        nonce: 42,
        ..Default::default()
    };

    // Transaction hash gets computed during this as well.
    tx.sign(&key);

    client
        .send_transaction(&tx)
        .await
        .map_err(|err| format!("Error during send  transaction to the node:{err}"))?;

    let read_tx = client
        .get_transaction(&tx.hash)
        .await
        .map_err(|err| format!("Error during send  get_transaction from the node:{err}"))?;

    read_tx
        .map(|read| tx.hash == read.hash)
        .ok_or("Error get_transaction doesn't return the right tx".to_string())?;

    Ok(())
}

fn create_tx_data(
    verified_file: Option<PathBuf>,
    tx_input_args: String,
    file_url_args: Option<String>,
    served_file_map: &HashMap<&PathBuf, String>,
) -> std::result::Result<ProgramMetadata, String> {
    let (image_file_name, image_file_url, image_file_checksum) = generate_tx_data(
        verified_file,
        tx_input_args,
        file_url_args,
        &served_file_map,
    )?;
    let mut data = ProgramMetadata {
        //TODO what is the role of the name?
        name: image_file_name.clone(),
        hash: Hash::default(),
        image_file_name: image_file_name,
        image_file_url: image_file_url.to_string(),
        image_file_checksum,
    };
    //TODO use a new to be sure the update is done during creation.
    data.update_hash();
    Ok(data)
}

fn generate_tx_data(
    verified_file: Option<PathBuf>,
    tx_input_args: String,
    file_url_args: Option<String>,
    served_file_map: &HashMap<&PathBuf, String>,
) -> std::result::Result<(String, String, String), String> {
    verified_file
        .and_then(|path| {
            //case 1 prover is a file name
            extract_hash_from_file_content(&path)
                .and_then(|file_hash| {
                    path.file_name()
                        .and_then(|file_name| file_name.to_str())
                        .map(|file_name| (file_name.to_string(), file_hash))
                })
                .and_then(|(file_name, file_hash)| {
                    served_file_map
                        .get(&path)
                        .map(|url| Ok((file_name, url.to_string(), file_hash)))
                })
        })
        .or_else(|| {
            //case 2 prover is a hash
            //try to detect if the parameters is a hash
            let res = hex::decode(&tx_input_args)
                .map_err(|err| format!("error during Hash decoding:{err}"))
                .and_then(|val| {
                    <[u8; 32]>::try_from(val)
                        .map_err(|err| format!("error Hash binary vec into array conv:{err:?}"))?;
                    Ok(())
                })
                .and_then(|_| {
                    //Get file name from url or define as file hash
                    let filename = file_url_args
                        .as_ref()
                        .and_then(|url| get_last_segment_from_url(url))
                        .unwrap_or(tx_input_args.to_string());
                    file_url_args
                        .map(|img_url| (filename, img_url, tx_input_args))
                        .ok_or("Image url not provided with Hash".to_string())
                });
            Some(res)
        })
        //never None
        .unwrap()
}

fn get_last_segment_from_url(url: &str) -> Option<String> {
    url::Url::parse(url).ok().and_then(|s| {
        s.path_segments()
            .and_then(|iter| iter.last().map(String::from))
    })
}

//Verifiy that the path exist and it's a file.
fn verify_file_path(file_path: &str) -> Option<PathBuf> {
    let path: PathBuf = file_path.into();
    path.try_exists()
        .map(|res| res && path.is_file())
        .ok()
        .and_then(|present| present.then(|| path))
}

//TODO add utillity function to Hash.
fn extract_hash_from_file_content(path: &PathBuf) -> Option<String> {
    let mut hasher = blake3::Hasher::new();
    let fd = std::fs::File::open(path).ok()?;
    hasher.update_reader(fd).ok()?;
    let checksum = hasher.finalize();
    Some(checksum.to_string())
}
