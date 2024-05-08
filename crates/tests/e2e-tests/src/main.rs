use clap::Parser;
use gevulot_node::{
    rpc_client::{RpcClient, RpcClientBuilder},
    types::{
        program::ResourceRequest,
        transaction::{Payload, ProgramData, ProgramMetadata, Workflow, WorkflowStep},
        Hash, Transaction,
    },
};
use libsecp256k1::SecretKey;
use server::FileServer;
use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::time::sleep;

mod server;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Parser, Debug)]
#[clap(author = "Gevulot Team", version, about, long_about = None)]
pub struct ArgConfiguration {
    #[clap(short, long)]
    pub prover_img: PathBuf,
    #[clap(short, long)]
    pub verifier_img: PathBuf,
    #[clap(short, long, default_value = "http://localhost:9944")]
    pub json_rpc_url: String,
    #[clap(short, long, default_value = "localkey.pki")]
    pub key_file: PathBuf,
    #[clap(short, long, default_value = "127.0.0.1:0")]
    pub listen_addr: SocketAddr,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cfg = ArgConfiguration::parse();
    let client = RpcClientBuilder::default().build(cfg.json_rpc_url)?;
    let file_server = Arc::new(FileServer::new(cfg.listen_addr).await);

    let bs = std::fs::read(cfg.key_file)?;
    let key = SecretKey::parse_slice(&bs)?;

    let (_tx_hash, prover_hash, verifier_hash) = deploy_programs(
        &client,
        file_server.clone(),
        &key,
        "e2e-test",
        &cfg.prover_img,
        &cfg.verifier_img,
    )
    .await
    .expect("deploy");

    for nonce in 1..2 {
        send_proving_task(&client, &key, nonce, &prover_hash, &verifier_hash)
            .await
            .expect("send proving task");
    }

    sleep(Duration::from_secs(360)).await;

    Ok(())
}

async fn deploy_programs(
    client: &RpcClient,
    file_server: Arc<FileServer>,
    key: &SecretKey,
    deployment_name: &str,
    prover_img: &Path,
    verifier_img: &Path,
) -> Result<(Hash, Hash, Hash)> {
    let prover =
        from_img_file_to_metadata(prover_img, &file_server.register_file(prover_img).await);
    let verifier =
        from_img_file_to_metadata(verifier_img, &file_server.register_file(verifier_img).await);
    let tx = Transaction::new(
        Payload::Deploy {
            name: deployment_name.to_string(),
            prover: prover.clone(),
            verifier: verifier.clone(),
        },
        key,
    );

    client
        .send_transaction(&tx)
        .await
        .expect("send_transaction");

    let read_tx = client
        .get_transaction(&tx.hash)
        .await
        .expect("get_transaction");

    assert_eq!(tx.hash, read_tx.hash.into());

    Ok((tx.hash, prover.hash, verifier.hash))
}

async fn send_proving_task(
    client: &RpcClient,
    key: &SecretKey,
    nonce: u64,
    prover_hash: &Hash,
    verifier_hash: &Hash,
) -> Result<Hash> {
    let proving_step = WorkflowStep {
        program: *prover_hash,
        args: vec!["--nonce".to_string(), nonce.to_string()],
        inputs: vec![],
    };

    let verifying_step = WorkflowStep {
        program: *verifier_hash,
        args: vec!["--nonce".to_string(), nonce.to_string()],
        inputs: vec![ProgramData::Output {
            source_program: *prover_hash,
            file_name: "/workspace/proof.dat".to_string(),
        }],
    };

    let tx = Transaction::new(
        Payload::Run {
            workflow: Workflow {
                steps: vec![proving_step, verifying_step],
            },
        },
        key,
    );

    client
        .send_transaction(&tx)
        .await
        .expect("send_transaction");

    Ok(tx.hash)
}

fn from_img_file_to_metadata(img_file: &Path, img_file_url: &str) -> ProgramMetadata {
    let mut hasher = blake3::Hasher::new();
    let fd = std::fs::File::open(img_file).expect("open");
    hasher.update_reader(fd).expect("checksum");
    let checksum = hasher.finalize();

    let file_name = img_file
        .file_name()
        .expect("file name")
        .to_str()
        .unwrap()
        .to_string();

    let mut program = ProgramMetadata {
        name: file_name.clone(),
        hash: Hash::default(),
        image_file_name: file_name,
        image_file_url: img_file_url.to_string(),
        image_file_checksum: checksum.to_string(),
        resource_requirements: Some(ResourceRequest {
            cpus: 1,
            mem: 128,
            gpus: 0,
        }),
    };

    program.update_hash();
    program
}
