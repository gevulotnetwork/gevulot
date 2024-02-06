use clap::Parser;
use clap::Subcommand;
use gevulot_node::rpc_client::RpcClient;
use gevulot_node::types::Hash;
use gevulot_node::types::TransactionTree;
use std::net::SocketAddr;
use std::path::PathBuf;

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
enum ConfCommands {
    /// Generate a private key file using --keyfile option.
    GenerateKey,

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
        /// url to get the prover image. If provided the prover will use this URL to get the prover image file. If not the cli tool starts a local HTTP server to serve the file to the node.
        #[clap(long = "proverimgurl", value_name = "PROVER URL")]
        prover_img_url: Option<String>,
        /// url to get the verifier image. If provided the verifier will use this URL to get the verifier image. If not the cli tool starts a local HTTP server to serve the file to the node.
        #[clap(long = "verifierimgurl", value_name = "VERIFIER URL")]
        verifier_img_url: Option<String>,
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
        ///     inputs: [{"Output":{"source_program":"Program Hash","file_name":"filename"}}],
        ///     , ...
        /// }]
        /// Example for proving and verification:
        /// --tasks '[
        /// {"program":"9616d42b0d82c1ed06eab8eaa26680261ad831012bbf3ad8303738a53bf85c7c","cmd_args":[{"name":"--nonce","value":"42"}],"inputs":[]}
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
    PrintTxTree {
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
        ConfCommands::Deploy {
            name,
            prover,
            verifier,
            prover_img_url,
            verifier_img_url,
            listen_addr,
        } => {
            println!("Start prover / verifier deployement");
            match gevulot_cli::run_deploy_command(
                client,
                args.keyfile,
                name,
                prover,
                verifier,
                prover_img_url,
                verifier_img_url,
                listen_addr,
            )
            .await
            {
                Ok((tx_hash, prover_hash, verifier_hash)) => println!("Prover / Verifier deployed correctly. Prover hash:{prover_hash} Verifier hash:{verifier_hash}. Tx Hash:{tx_hash}"),
                Err(err) => println!("An error occurs during Prover / Verifier deployement :{err}"),
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
    }
}

fn print_tx_tree(tree: &TransactionTree, indentation: u16) {
    match tree {
        TransactionTree::Root { children, hash } => {
            println!("Root: {hash}");
            children
                .iter()
                .for_each(|x| print_tx_tree(&x, indentation + 1));
        }
        TransactionTree::Node { children, hash } => {
            println!(
                "{}Node: {hash}",
                (0..indentation).map(|_| "\t").collect::<String>()
            );
            children
                .iter()
                .for_each(|x| print_tx_tree(&x, indentation + 1));
        }
        TransactionTree::Leaf { hash } => {
            println!(
                "{}Leaf: {hash}",
                (0..indentation).map(|_| "\t").collect::<String>()
            );
        }
    }
}
