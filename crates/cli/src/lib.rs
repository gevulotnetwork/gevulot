use crate::file::FileData;
use gevulot_node::types::transaction::ProgramMetadata;
use gevulot_node::types::Hash;
use gevulot_node::{
    rpc_client::RpcClient,
    types::{
        transaction::{Payload, ProgramData, Workflow, WorkflowStep},
        Transaction,
    },
};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::path::PathBuf;

mod file;
pub mod keyfile;
mod server;

pub const HTTP_DEFAULT_ADDR: &str = "127.0.0.1:8080";

type BoxResult<T> = std::result::Result<T, Box<dyn std::error::Error>>;

pub async fn calculate_hash_command(file_path: &PathBuf) -> BoxResult<String> {
    Ok(crate::file::extract_hash_from_file_content(file_path)
        .ok_or_else(|| format!("File not found:{:?}", file_path))?
        .to_string())
}

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

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub enum JsonProgramData {
    Input {
        //path to the local file or Hash os the distant file
        file: String,
        //optional url to the file if the file hash is provided by file
        file_url: Option<String>,
    },
    Output {
        source_program: String,
        file_name: String,
    },
}

fn get_file_data_from_json(prog_data: &JsonProgramData) -> Option<(String, Option<String>)> {
    match prog_data {
        JsonProgramData::Input { file, file_url } => Some((file.clone(), file_url.clone())),
        _ => None,
    }
}

pub async fn run_exec_command(
    client: RpcClient,
    keyfile: PathBuf,
    json_tasks: String,
    listen_addr: Option<SocketAddr>,
) -> BoxResult<String> {
    let key = keyfile::read_key_file(&keyfile).map_err(|err| {
        format!(
            "Error during key file:{} reading:{err}",
            keyfile.to_str().unwrap_or("")
        )
    })?;

    let json_args = serde_json::from_str::<Vec<JsonExecArgs>>(&json_tasks)
        .map_err(|err| format!("Json decoding error :{err} with :{json_tasks}"))?;

    //create input file workflowstep download data
    let input_args_iter = json_args
        .iter()
        .flat_map(|args| args.inputs.clone())
        .filter_map(|input| get_file_data_from_json(&input));
    let (file_data, server_jh) =
        crate::file::prepare_files_download(listen_addr, input_args_iter).await?;

    //reconstruct all step with file_data
    let mut steps = vec![];
    let mut counter = 0;
    for args in json_args {
        let input_data = args
            .inputs
            .into_iter()
            .map(|input| {
                let ret = match input {
                    JsonProgramData::Input { .. } => {
                        let input_data = &file_data[counter];
                        counter += 1;

                        ProgramData::Input {
                            file_name: input_data.filename.clone(),
                            file_url: input_data.url.clone(),
                            checksum: input_data.checksum.to_string(),
                        }
                    }
                    JsonProgramData::Output {
                        source_program,
                        file_name,
                    } => ProgramData::Output {
                        source_program: (&(hex::decode(source_program)?)[..]).into(),
                        file_name,
                    },
                };
                Ok(ret)
            })
            .collect::<BoxResult<Vec<_>>>()?;
        let step = WorkflowStep {
            program: (&(hex::decode(args.program)
                .map_err(|err| format!("program decoding hash error:{err}"))?)[..])
                .into(),
            args: args
                .cmd_args
                .into_iter()
                .flat_map(<[String; 2]>::from)
                .collect(),
            inputs: input_data,
        };

        steps.push(step);
    }
    let tx = Transaction::new(
        Payload::Run {
            workflow: Workflow { steps },
        },
        &key,
    );

    let tx_hash = send_transaction(&client, &tx).await?;

    if let Some(server_jh) = server_jh {
        let _ = server_jh.await;
    }
    Ok(tx_hash.to_string())
}

#[allow(clippy::too_many_arguments)]
pub async fn run_deploy_command(
    client: RpcClient,
    keyfile: PathBuf,
    name: String,
    prover: String,
    verifier: String,
    prover_img_url: Option<String>,
    verifier_img_url: Option<String>,
    listen_addr: SocketAddr,
) -> BoxResult<(String, String, String)> {
    let key = keyfile::read_key_file(&keyfile).map_err(|err| {
        format!(
            "Error during key file:{} reading:{err}",
            keyfile.to_str().unwrap_or("")
        )
    })?;

    let (mut file_data, server_jh) = crate::file::prepare_files_download(
        Some(listen_addr),
        vec![(prover, prover_img_url), (verifier, verifier_img_url)].into_iter(),
    )
    .await?;

    let prover_data: FileData = file_data.swap_remove(0).into();
    let verifier_data: FileData = file_data.swap_remove(0).into();

    let prover_prg_data: ProgramMetadata = prover_data.into();
    let prover_prg_hash = prover_prg_data.hash;
    let verifier_prg_data: ProgramMetadata = verifier_data.into();
    let verifier_prg_hash = verifier_prg_data.hash;

    let tx = Transaction::new(
        Payload::Deploy {
            name,
            prover: prover_prg_data,
            verifier: verifier_prg_data,
        },
        &key,
    );

    let tx_hash = send_transaction(&client, &tx).await?;

    if let Some(server_jh) = server_jh {
        let _ = server_jh.await;
    }

    Ok((
        tx_hash.to_string(),
        prover_prg_hash.to_string(),
        verifier_prg_hash.to_string(),
    ))
}

async fn send_transaction(client: &RpcClient, tx: &Transaction) -> Result<Hash, String> {
    client
        .send_transaction(tx)
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
    Ok(tx_hash)
}
