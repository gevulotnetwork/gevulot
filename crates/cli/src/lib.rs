use gevulot_node::{
    rpc_client::RpcClient,
    types::{
        transaction::{Payload, ProgramData, ProgramMetadata, Workflow, WorkflowStep},
        Hash, Transaction,
    },
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;

pub mod keyfile;
mod server;

type BoxResult<T> = std::result::Result<T, Box<dyn std::error::Error>>;

pub async fn calculate_hash_command(file_path: &PathBuf) -> BoxResult<String> {
    Ok(extract_hash_from_file_content(file_path)
        .ok_or_else(|| format!("File not found:{:?}", file_path))?)
}

// {
//     program: "Program Hash",
//     cmd_args: [ {name: "args name", value:"args value"}, ...],
//     inputs: vec![],
// }
#[derive(Serialize, Deserialize, Debug, Clone)]
struct JsonCmdArgs {
    name: String,
    value: String,
}

impl From<JsonCmdArgs> for [String; 2] {
    fn from(args: JsonCmdArgs) -> Self {
        [args.name, args.value]
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct JsonExecArgs {
    program: String,
    cmd_args: Vec<JsonCmdArgs>,
    inputs: Vec<JsonProgramData>,
}

impl TryFrom<JsonExecArgs> for WorkflowStep {
    type Error = String;

    fn try_from(data: JsonExecArgs) -> Result<Self, Self::Error> {
        Ok(WorkflowStep {
            program: (&(hex::decode(data.program)
                .map_err(|err| format!("program decoding hash error:{err}"))?)[..])
                .into(),
            args: data
                .cmd_args
                .into_iter()
                .flat_map(<[String; 2]>::from)
                .collect(),
            inputs: data
                .inputs
                .into_iter()
                .map(|i| i.try_into())
                .collect::<Result<Vec<ProgramData>, _>>()
                .map_err(|err| format!("Source program decoding hash error:{err}"))?,
        })
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub enum JsonProgramData {
    Input {
        file_name: String,
        file_url: String,
        checksum: String,
    },
    Output {
        source_program: String,
        file_name: String,
    },
}

impl TryFrom<JsonProgramData> for ProgramData {
    type Error = hex::FromHexError;

    fn try_from(data: JsonProgramData) -> Result<Self, Self::Error> {
        match data {
            JsonProgramData::Input {
                file_name,
                file_url,
                checksum,
            } => Ok(ProgramData::Input {
                file_name,
                file_url,
                checksum,
            }),
            JsonProgramData::Output {
                source_program,
                file_name,
            } => Ok(ProgramData::Output {
                source_program: (&(hex::decode(source_program)?)[..]).into(),
                file_name,
            }),
        }
    }
}

pub async fn run_exec_command(
    client: RpcClient,
    keyfile: PathBuf,
    json_tasks: String,
) -> BoxResult<String> {
    let key = keyfile::read_key_file(&keyfile).map_err(|err| {
        format!(
            "Error during key file:{} reading:{err}",
            keyfile.to_str().unwrap_or("")
        )
    })?;

    //    serde_json::from_str::<JsonExecArgs>("{\"program\":\"step1\",\"cmd_args\":[{\"name\":\"--nonce\",\"value\":\"42\"}],\"inputs\":[]}").map_err(|err| format!("Json test decoding error :{err}")).unwrap();

    let steps = serde_json::from_str::<Vec<JsonExecArgs>>(&json_tasks)
        .map_err(|err| format!("Json decoding error :{err} with :{json_tasks}"))?
        .into_iter()
        .map(|t| t.try_into())
        .collect::<Result<Vec<_>, _>>()?;

    let mut tx = Transaction {
        payload: Payload::Run {
            workflow: Workflow { steps },
        },
        //TODO define nonce use
        nonce: 42,
        ..Default::default()
    };
    tx.sign(&key);

    client.send_transaction(&tx).await?;
    Ok(tx.hash.to_string())
}

#[allow(clippy::too_many_arguments)]
pub async fn run_deploy_command(
    client: RpcClient,
    keyfile: PathBuf,
    name: String,
    prover: String,
    verifier: String,
    proverimgurl: Option<String>,
    verifierimgurl: Option<String>,
    listen_addr: SocketAddr,
) -> BoxResult<(String, String, String)> {
    let key = keyfile::read_key_file(&keyfile).map_err(|err| {
        format!(
            "Error during key file:{} reading:{err}",
            keyfile.to_str().unwrap_or("")
        )
    })?;
    let exist_prover_path = verify_file_path(&prover);
    let exist_verifier_path = verify_file_path(&verifier);

    //start the local server if needed. Local file provided
    let server_files_path: Vec<PathBuf> = exist_prover_path
        .iter()
        .chain(exist_verifier_path.iter())
        .cloned()
        .collect();
    let (served_file_map, server_jh) = if !server_files_path.is_empty() {
        //some local files start the server.
        server::serve_file(listen_addr, &server_files_path)
            .await
            .map(|(map, jh)| (map, Some(jh)))
            .map_err(|err| {
                format!(
                    "Couldn't bind to specified bind address:{} because {err}",
                    listen_addr
                )
            })?
    } else {
        (HashMap::new(), None)
    };

    //create tx data
    let Ok(prover_data) = create_tx_data(exist_prover_path, prover, proverimgurl, &served_file_map)
    else {
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
        return Err(
            "Wrong specified verifier image file path. File can't be read."
                .to_string()
                .into(),
        );
    };

    let prover_hash = prover_data.hash.to_string();
    let verifier_hash = verifier_data.hash.to_string();

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

    let tx_hash = read_tx
        .as_ref()
        .and_then(|read| (tx.hash == read.hash).then_some(read.hash))
        .ok_or_else(|| {
            format!(
                "Error get_transaction doesn't return the right tx send tx:{} read tx:{:?}",
                tx.hash, read_tx
            )
        })?;

    if let Some(server_jh) = server_jh {
        let _ = server_jh.await;
    }

    Ok((tx_hash.to_string(), prover_hash, verifier_hash))
}

fn create_tx_data(
    verified_file: Option<PathBuf>,
    tx_input_args: String,
    file_url_args: Option<String>,
    served_file_map: &HashMap<&PathBuf, String>,
) -> std::result::Result<ProgramMetadata, String> {
    let (image_file_name, image_file_url, image_file_checksum) =
        generate_tx_data(verified_file, tx_input_args, file_url_args, served_file_map)?;
    let mut data = ProgramMetadata {
        //TODO what is the role of the name?
        name: image_file_name.clone(),
        hash: Hash::default(),
        image_file_name,
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
        .and_then(|present| present.then_some(path))
}

//TODO add utillity function to Hash.
fn extract_hash_from_file_content(path: &PathBuf) -> Option<String> {
    let mut hasher = blake3::Hasher::new();
    let fd = std::fs::File::open(path).ok()?;
    hasher.update_reader(fd).ok()?;
    let checksum = hasher.finalize();
    Some(checksum.to_string())
}
