use clap::Parser;
use clap::Subcommand;
use clap_num::number_range;
use gevulot_node::rpc_client::RpcClient;
use gevulot_node::types::program::ResourceRequest;
use gevulot_node::types::Hash;
use gevulot_node::types::TransactionTree;
use std::net::SocketAddr;
use std::path::PathBuf;
use libsecp256k1::PublicKey;

#[derive(Parser, Debug)]
#[clap(author = "Gevulot Team", version, about, long_about = None)]
pub struct ArgConfiguration {
    /// RPC url of the Gevulot node
    #[clap(
        short,
        long = "jsonurl",
        default_value = "http://localhost:9944",
        value_name = "URL"
    )]
    json_url: String,
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
#[allow(clippy::large_enum_variant)]
enum ConfCommands {
    /// Generate a private key file using --keyfile option.
    GenerateKey,
    /// Print a public key using --keyfile option.
    PrintPublicKey,

    /// Deploy prover and verifier.
    #[command(arg_required_else_help = true)]
    Deploy {
        /// name of the deployment.
        #[clap(short, long, value_name = "DEPLOYMENT NAME")]
        name: String,
        /// file path containing the program image of the prover to deploy or the hash  of the prover image file (--prover-img-url is mandatory in this case). If the  file doesn't exist, the parameter is used as a hash.
        #[clap(short, long, value_name = "PROVER FILE or HASH")]
        prover: String,
        /// file path containing the program image of the verifier to deploy or the hash  of the verifier image file (--verifier-img-url is mandatory in this case). If the file doesn't exist, the parameter is used as a hash.
        #[clap(short, long, value_name = "VERIFIER FILE or HASH")]
        verifier: String,
        /// name of the prover.
        #[clap(long = "provername", value_name = "PROVER NAME")]
        prover_name: Option<String>,
        /// name of the verifier.
        #[clap(long = "verifiername", value_name = "VERIFIER NAME")]
        verifier_name: Option<String>,
        /// url to get the prover image. If provided the prover will use this URL to get the prover image file. If not the cli tool starts a local HTTP server to serve the file to the node.
        #[clap(long = "proverimgurl", value_name = "PROVER URL")]
        prover_img_url: Option<String>,
        /// url to get the verifier image. If provided the verifier will use this URL to get the verifier image. If not the cli tool starts a local HTTP server to serve the file to the node.
        #[clap(long = "verifierimgurl", value_name = "VERIFIER URL")]
        verifier_img_url: Option<String>,
        /// number of cpus to allocate for the proving task.
        #[clap(long = "provercpus", value_name = "PROVER CPUS")]
        prover_cpus: Option<u64>,
        /// number of megabytes to allocate for the proving task.
        #[clap(long = "provermem", value_name = "PROVER MEM")]
        prover_mem: Option<u64>,
        /// number of gpus to allocate for the proving task (currently only 0 or 1 allowed).
        #[clap(long = "provergpus", value_name = "PROVER GPUS", value_parser = gpus_parser)]
        prover_gpus: Option<u64>,
        /// number of cpus to allocate for the proving task.
        #[clap(long = "verifiercpus", value_name = "VERIFIER CPUS")]
        verifier_cpus: Option<u64>,
        /// number of megabytes to allocate for the proving task.
        #[clap(long = "verifiermem", value_name = "VERIFIER MEM")]
        verifier_mem: Option<u64>,
        /// number of gpus to allocate for the proving task (currently only 0 or 1 allowed).
        #[clap(long = "verifiergpus", value_name = "VERIFIER GPUS", value_parser = gpus_parser)]
        verifier_gpus: Option<u64>,
        /// Address the local http server use by the node to download images.
        #[clap(
            short,
            long,
            default_value = "127.0.0.1:8080",
            value_name = "LOCAL SERVER BIND ADDR"
        )]
        listen_addr: SocketAddr,
    },

    /// Execute the list of task in the order one after the other.
    #[command(arg_required_else_help = true)]
    Exec {
        /// Optional Address of the local http server use by the node to download input file.
        #[clap(
            short,
            long,
            default_value = "127.0.0.1:8080",
            value_name = "LOCAL SERVER BIND ADDR"
        )]
        listen_addr: Option<SocketAddr>,
        /// array of Json task definition.
        /// Json format of the task data:
        /// [{
        ///     program: "Program Hash",
        ///     cmd_args: [ {name: "args name", value:"args value"}, ...],
        ///     inputs: [
        ///         {"Input":{"local_path":"<Path to the local file>","vm_path":"<path to read the file in the VM", "file_url":"<Optional file url if not local. In this case local_path contains the file checksum"},
        ///         {"Output":{"source_program":"Program Hash","file_name":"<file path where the file is written in the VM"}}],
        ///     , ...
        /// }]
        /// Example for proving and verification:
        /// --tasks '[
        /// {"program":"9616d42b0d82c1ed06eab8eaa26680261ad831012bbf3ad8303738a53bf85c7c","cmd_args":[{"name":"--nonce","value":"42"}],"inputs":[{"Input":{"local_path":"witness.txt","vm_path":"/workspace/witness.txt"}}]}
        ///,
        ///{
        /// "program":"37ef718f473a96e2dd56ac27fc175bfa08f4a30e34bdff5802e2f5071265a942",
        /// "cmd_args":[{"name":"--nonce2","value":"45"},{"name":"--nonce3","value":"46"}]
        ///,"inputs":[{"Output":{"source_program":"9616d42b0d82c1ed06eab8eaa26680261ad831012bbf3ad8303738a53bf85c7c","file_name":"/workspace/proof.dat"}}]
        /// }
        ///]'
        #[clap(short, long, value_name = "TASK ARRAY")]
        tasks: String,
    },
    /// Return the tree of executed tx associated to the specified Run Tx (Exec cmd).
    #[command(arg_required_else_help = true)]
    PrintTxTree {
        /// Hash of the Run Tx to look for.
        hash: String,
    },

    /// Return the Tx with the specified Hash.
    #[command(arg_required_else_help = true)]
    GetTx {
        /// Hash of the Run Tx to look for.
        hash: String,
    },

    /// Calculate the Hash of the specified file.
    #[command(arg_required_else_help = true)]
    CalculateHash {
        /// Path to the file to hash.
        #[clap(short, long, value_name = "FILE PATH")]
        file: PathBuf,
    },
}

fn gpus_parser(s: &str) -> Result<u64, String> {
    number_range(s, 0, 1)
}

fn resource_requirements(
    cpus: Option<u64>,
    mem: Option<u64>,
    gpus: Option<u64>,
) -> Option<ResourceRequest> {
    if cpus.is_none() && mem.is_none() && gpus.is_none() {
        return None;
    }

    let mut req = ResourceRequest::default();
    if cpus.is_some() {
        req.cpus = cpus.unwrap();
    }

    if mem.is_some() {
        req.mem = mem.unwrap();
    }

    if gpus.is_some() {
        req.gpus = gpus.unwrap();
    }

    Some(req)
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let args = ArgConfiguration::parse();

    let client = RpcClient::new(args.json_url);

    match args.command {
        ConfCommands::GenerateKey => match gevulot_cli::keyfile::create_key_file(&args.keyfile) {
            Ok(pubkey) => println!(
                "Key generated  pubkey:{} and saved in file:{}",
                hex::encode(pubkey.serialize()),
                args.keyfile.to_str().unwrap_or("")
            ),
            Err(err) => println!("Error during key file creation:{err}"),
        },
        ConfCommands::PrintPublicKey => {
            match gevulot_cli::keyfile::read_key_file(&args.keyfile) {
                Ok(key) => {
                    let pubkey = PublicKey::from_secret_key(&key);
                    return println!(
                        "Extracted pubkey:{}",
                        hex::encode(pubkey.serialize())
                    )
                },
                Err(err) => println!("Error during key file access:{err}"),
            }
        }
        ConfCommands::Deploy {
            name,
            prover,
            verifier,
            prover_name,
            verifier_name,
            prover_img_url,
            verifier_img_url,
            prover_cpus,
            prover_mem,
            prover_gpus,
            verifier_cpus,
            verifier_mem,
            verifier_gpus,
            listen_addr,
        } => {
            let prover_reqs = resource_requirements(prover_cpus, prover_mem, prover_gpus);
            let verifier_reqs = resource_requirements(verifier_cpus, verifier_mem, verifier_gpus);

            println!("Start prover / verifier deployment");
            match gevulot_cli::run_deploy_command(
                client,
                args.keyfile,
                name,
                prover,
                verifier,
                prover_name,
                verifier_name,
                prover_img_url,
                verifier_img_url,
                prover_reqs,
                verifier_reqs,
                listen_addr,
            )
            .await
            {
                Ok((tx_hash, prover_hash, verifier_hash)) => println!("Prover / Verifier deployed correctly.\nProver hash:{prover_hash}\nVerifier hash:{verifier_hash}.\nTx Hash:{tx_hash}"),
                Err(err) => println!("An error occurs during Prover / Verifier deployment :{err}"),
            }
        }
        ConfCommands::Exec { tasks, listen_addr } => {
            match gevulot_cli::run_exec_command(client, args.keyfile, tasks, listen_addr).await {
                Ok(tx_hash) => println!("Programs send to execution correctly. Tx hash:{tx_hash}"),
                Err(err) => println!("An error occurs during send execution Tx :{err}"),
            }
        }
        ConfCommands::CalculateHash { file } => {
            match gevulot_cli::calculate_hash_command(&file).await {
                Ok(tx_hash) => println!("The hash of the file is: {tx_hash}"),
                Err(err) => println!("An error hash calculus Tx :{err}"),
            }
        }
        ConfCommands::PrintTxTree { hash } => {
            let hash = Hash::from(hash);
            match client.get_tx_tree(&hash).await {
                Ok(tx_tree) => print_tx_tree(&tx_tree, 0),
                Err(err) => println!("An error while fetching transaction tree: {err}"),
            };
        }
        ConfCommands::GetTx { hash } => {
            let hash = Hash::from(hash);
            match client
                .get_transaction(&hash)
                .await
                .and_then(|tx_output| serde_json::to_string(&tx_output).map_err(|err| err.into()))
            {
                Ok(output_json) => println!("{output_json}"),
                Err(err) => println!("An error while getting Tx : {err}"),
            };
        }
    }
}

fn print_tx_tree(tree: &TransactionTree, indentation: u16) {
    match tree {
        TransactionTree::Root { children, hash } => {
            println!("Root: {hash}");
            children
                .iter()
                .for_each(|x| print_tx_tree(x, indentation + 1));
        }
        TransactionTree::Node { children, hash } => {
            println!(
                "{}Node: {hash}",
                (0..indentation).map(|_| "\t").collect::<String>()
            );
            children
                .iter()
                .for_each(|x| print_tx_tree(x, indentation + 1));
        }
        TransactionTree::Leaf { hash } => {
            println!(
                "{}Leaf: {hash}",
                (0..indentation).map(|_| "\t").collect::<String>()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::*;

    #[test]
    fn test_default_resource_requirements() {
        assert_eq!(resource_requirements(None, None, None), None);
    }

    #[test]
    fn test_cpus_resource_requirements() {
        assert_eq!(
            resource_requirements(Some(16), None, None),
            Some(ResourceRequest {
                cpus: 16,
                ..Default::default()
            })
        );
    }

    #[test]
    fn test_mem_resource_requirements() {
        assert_eq!(
            resource_requirements(None, Some(24576), None),
            Some(ResourceRequest {
                mem: 24576,
                ..Default::default()
            })
        );
    }

    #[test]
    fn test_gpus_resource_requirements() {
        assert_eq!(
            resource_requirements(None, None, Some(1)),
            Some(ResourceRequest {
                gpus: 1,
                ..Default::default()
            })
        );
    }

    #[test]
    fn test_cpu_mem_requirements() {
        assert_eq!(
            resource_requirements(Some(4), Some(4096), None),
            Some(ResourceRequest {
                cpus: 4,
                mem: 4096,
                ..Default::default()
            })
        );
    }
}
