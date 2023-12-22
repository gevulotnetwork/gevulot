use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use clap::Parser;
use gevulot_node::{
    rpc_client::RpcClient,
    types::{
        transaction::{Payload, ProgramData, ProgramMetadata, Workflow, WorkflowStep},
        Hash, Transaction,
    },
};
use libsecp256k1::SecretKey;
use rand::{rngs::StdRng, SeedableRng};
use server::FileServer;
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
    #[clap(short, long, default_value = "127.0.0.1:0")]
    pub listen_addr: SocketAddr,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cfg = ArgConfiguration::parse();
    let client = RpcClient::new(cfg.json_rpc_url);
    let file_server = Arc::new(FileServer::new(cfg.listen_addr).await);

    let key = SecretKey::random(&mut StdRng::from_entropy());

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

    send_proving_task(&client, &key, &prover_hash, &verifier_hash)
        .await
        .expect("send proving task");

    sleep(Duration::from_secs(10)).await;

    Ok(())
}

async fn deploy_programs(
    client: &RpcClient,
    file_server: Arc<FileServer>,
    key: &SecretKey,
    deployment_name: &str,
    prover_img: &PathBuf,
    verifier_img: &PathBuf,
) -> Result<(Hash, Hash, Hash)> {
    let prover =
        from_img_file_to_metadata(prover_img, &file_server.register_file(prover_img).await);
    let verifier =
        from_img_file_to_metadata(verifier_img, &file_server.register_file(verifier_img).await);
    let mut tx = Transaction {
        payload: Payload::Deploy {
            name: deployment_name.to_string(),
            prover: prover.clone(),
            verifier: verifier.clone(),
        },
        nonce: 42,
        ..Default::default()
    };

    // Transaction hash gets computed during this as well.
    tx.sign(key);

    client
        .send_transaction(&tx)
        .await
        .expect("send_transaction");

    let read_tx = client
        .get_transaction(&tx.hash)
        .await
        .expect("get_transaction");

    assert!(read_tx.is_some());
    assert_eq!(tx, read_tx.unwrap());

    Ok((tx.hash, prover.hash, verifier.hash))
}

async fn send_proving_task(
    client: &RpcClient,
    key: &SecretKey,
    prover_hash: &Hash,
    verifier_hash: &Hash,
) -> Result<Hash> {
    let proving_step = WorkflowStep {
        program: prover_hash.clone(),
        args: vec![],
        inputs: vec![],
    };

    let verifying_step = WorkflowStep {
        program: verifier_hash.clone(),
        args: vec![],
        inputs: vec![ProgramData::Output {
            source_program: prover_hash.clone(),
            file_name: "/workspace/proof.dat".to_string(),
        }],
    };

    let mut tx = Transaction {
        payload: Payload::Run {
            workflow: Workflow {
                steps: vec![proving_step, verifying_step],
            },
        },
        nonce: 42,
        ..Default::default()
    };

    tx.sign(key);

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
    };

    program.update_hash();
    program
}
