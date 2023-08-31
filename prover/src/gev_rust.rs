use ark_bn254::Bn254;
use ark_groth16::api::do_prove as groth16_prove;
use ark_groth16::api::verify as groth16_verify;
use ark_marlin::api::do_prove as marlin_prove;
use ark_marlin::api::verify as marlin_verify;
use base64::{engine::general_purpose, Engine as _};
use filecoin_proofs::{
    generate_window_post_gevulot, generate_winning_post_gevulot, verify_window_post,
    verify_winning_post, FilProofInfo,
};
use gev_core::*;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    fs,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

use filecoin_proofs::constants::SectorShape512MiB;

type MerkleTree = SectorShape512MiB;

#[derive(Default, Clone, Serialize, Deserialize, Debug)]
pub struct Program {
    pub created: u64,
    pub r1cs: String,
}

#[derive(Default, Clone, Deserialize, Debug)]
pub struct UserInputs {
    pub inputs: Vec<u64>,
}

pub fn on_deploy(circuit_file: impl AsRef<Path>) -> Result<String, GevulotError> {
    println!("on_deploy: {:?}", &circuit_file.as_ref().display());
    let file = std::fs::read(circuit_file).map_err(|_| GevulotError::ErrorIo)?;
    println!("  read in {} bytes", file.len());
    let r1cs = general_purpose::STANDARD_NO_PAD.encode(&file);

    let created = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;

    let program_id = Uuid::new_v4();
    println!("  program id: {:?}", program_id);
    let program_path = format!("deployments/{program_id}.json");
    println!("  program path: {}", program_path);

    let program = json!(Program { created, r1cs }).to_string();
    println!("  program len: {}", program.len());
    std::fs::write(program_path, program).map_err(|_| GevulotError::ErrorIo)?;
    Ok(program_id.to_string())
}

pub fn on_prove(
    algorithm: GevulotAlg,
    program_id: &String,
    witness_file: &Option<impl AsRef<Path>>,
    proof_file: impl AsRef<Path>,
) -> Result<ProverResponse, GevulotError> {
    println!(
        "on_prove: program id {}, proof file {:?}",
        program_id,
        proof_file.as_ref().display()
    );

    if algorithm == GevulotAlg::WindowPost || algorithm == GevulotAlg::WinningPost {
        return on_prove_filecoin(algorithm, program_id, proof_file);
    }

    // Load program
    let program_path = format!("deployments/{program_id}.json");
    let jprogram = std::fs::read_to_string(program_path).map_err(|_| GevulotError::ErrorIo)?;
    let program: Program =
        serde_json::from_str(&jprogram).map_err(|_| GevulotError::CanonicalDeserializeError)?;
    let r1cs = program.r1cs;

    // Load witness file
    let witness_data =
        std::fs::read(witness_file.as_ref().unwrap()).map_err(|_| GevulotError::ErrorIo)?;
    let wtns = general_purpose::STANDARD_NO_PAD.encode(witness_data);

    // Prep prover inputs
    let prover_inputs = ProverInputs { r1cs, wtns };

    // Create the proof
    let prover_response = match algorithm {
        GevulotAlg::Marlin => marlin_prove(prover_inputs),
        GevulotAlg::Groth16 => groth16_prove::<Bn254>(prover_inputs),
        _ => {
            panic!("unnhandled algorithm");
        }
    }
    .unwrap();

    println!("prover_response.header: {:?}", prover_response.header);

    if prover_response.proof_info.is_some() {
        let output = json!(&prover_response.proof_info.as_ref().unwrap()).to_string();
        fs::write(proof_file, output).map_err(|_| GevulotError::ErrorIo)?;
    }
    Ok(prover_response)
}

pub fn on_prove_filecoin(
    algorithm: GevulotAlg,
    proof_inputs: &String,
    proof_file: impl AsRef<Path>,
) -> Result<ProverResponse, GevulotError> {
    println!(
        "on_prove: proof_inputs {}, proof file {:?}",
        proof_inputs,
        proof_file.as_ref().display()
    );

    // Load inputs
    let inputs_path = format!("deployments/{proof_inputs}.json");
    let jproofinputs = std::fs::read_to_string(inputs_path).unwrap();
    let fil_proof_info: FilProofInfo = serde_json::from_str(&jproofinputs).unwrap();
    let result = match algorithm {
        GevulotAlg::WindowPost => generate_window_post_gevulot::<MerkleTree>(fil_proof_info),
        GevulotAlg::WinningPost => generate_winning_post_gevulot::<MerkleTree>(fil_proof_info),
        _ => {
            panic!("should not be here");
        }
    };

    if result.is_err() {
        let gev_error: GevulotError = match algorithm {
            GevulotAlg::WindowPost => GevulotError::FilecoinWindowPostError,
            GevulotAlg::WinningPost => GevulotError::FilecoinWindowPostError,
            _ => {
                panic!("should not be here");
            }
        };
        return Err(gev_error);
    }

    let fil_proof_info = result.unwrap();
    let proof_info = ProofInfo {
        algorithm: algorithm.to_string(),
        proof: json!(fil_proof_info).to_string(),
        vk: "empty".to_owned(),
    };

    let prover_response = ProverResponse {
        header: ResponseHeader {
            success: true,
            message: PROVER_SUCCESS.to_string(),
        },
        proof_info: Some(proof_info),
    };

    if prover_response.proof_info.is_some() {
        let output = json!(&prover_response.proof_info.as_ref().unwrap()).to_string();
        fs::write(proof_file, output).map_err(|_| GevulotError::ErrorIo)?;
    }
    Ok(prover_response)
}

pub fn on_verify(
    proof_file: impl AsRef<Path>,
    user_inputs_file: &Option<impl AsRef<Path>>,
) -> Result<VerifyResponse, GevulotError> {
    // let user_inputs_file = user_inputs_file.as_ref().unwrap();
    println!("on_verify: proof file {:?} ", proof_file.as_ref().display(),);

    // Load proof file
    let jproof = std::fs::read_to_string(proof_file).map_err(|_| GevulotError::ErrorIo)?;
    let proof_info: ProofInfo =
        serde_json::from_str(&jproof).expect("could not deserialize ProofInfo");
    let algorithm = GevulotAlg::from(proof_info.algorithm.as_str());

    // Load user inputs
    let inputs: Vec<u64> = match user_inputs_file {
        Some(pathbuf) => {
            let juserinputs = std::fs::read_to_string(pathbuf).unwrap();
            let user_inputs: UserInputs = serde_json::from_str(&juserinputs)
                .map_err(|_| GevulotError::CanonicalDeserializeError)
                .unwrap();
            user_inputs.inputs
        }
        None => vec![0_u64],
    };

    // Do the verification
    let verify_response = match algorithm {
        GevulotAlg::Marlin => {
            let verify_inputs = VerifyInputs { inputs, proof_info };
            marlin_verify(verify_inputs)
        }
        GevulotAlg::Groth16 => {
            let verify_inputs = VerifyInputs { inputs, proof_info };
            groth16_verify::<Bn254>(verify_inputs)
        }
        GevulotAlg::WindowPost => {
            let fil_proof_info: FilProofInfo = serde_json::from_str(&proof_info.proof).unwrap();
            let result = verify_window_post::<MerkleTree>(fil_proof_info);
            if result.is_err() {
                return Err(GevulotError::FilecoinWindowPostError);
            };
            Ok(VerifyResponse {
                header: ResponseHeader {
                    success: result.unwrap(),
                    message: VERIFICATION_SUCCESS.to_owned(),
                },
            })
        }
        GevulotAlg::WinningPost => {
            let fil_proof_info: FilProofInfo = serde_json::from_str(&proof_info.proof).unwrap();
            let result = verify_winning_post::<MerkleTree>(fil_proof_info);
            if result.is_err() {
                return Err(GevulotError::FilecoinWinningPostError);
            };
            Ok(VerifyResponse {
                header: ResponseHeader {
                    success: result.unwrap(),
                    message: VERIFICATION_SUCCESS.to_owned(),
                },
            })
        }
    };

    println!("verify_response: {:?}", verify_response);
    verify_response
}

#[test]
fn test_rust_deploy() {
    println!("test_rust_deploy");
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let program_id = on_deploy("test-data/sudoku.r1cs").unwrap();
    let program_path = format!("deployments/{program_id}.json");

    let jprogram = std::fs::read_to_string(program_path).unwrap();
    let program: Program = serde_json::from_str(&jprogram).expect("could not deserialize Program");
    assert!(program.created > t);
    assert!(program.created - t < 100);
    assert_eq!(program.r1cs.len(), 1006720);
}

#[test]
fn test_rust_provers() {
    println!("test_rust_provers");
    let program_id = on_deploy("test-data/sudoku.r1cs").unwrap();
    let witness_file = "test-data/sudoku.wtns";
    let proof_file = "proof.json";
    let proof_systems = [GevulotAlg::Marlin, GevulotAlg::Groth16];
    for ps in proof_systems {
        let data = match ps {
            GevulotAlg::Marlin => [1546, 1831, 1159, 1373],
            GevulotAlg::Groth16 => [342, 54734, 256, 41050],
            _ => [0, 0, 0, 0],
        };
        let prover_response = on_prove(ps, &program_id, &Some(witness_file), proof_file).unwrap();
        let output = json!(prover_response.proof_info).to_string();
        fs::write(proof_file, output).unwrap();
        assert!(prover_response.header.success);
        assert!(prover_response.header.message.eq(PROVER_SUCCESS));
        let jproof = std::fs::read_to_string(proof_file).unwrap();
        let proof_info: ProofInfo =
            serde_json::from_str(&jproof).expect("could not deserialize ProofInfo");
        assert_eq!(proof_info.proof.len(), data[0]);
        assert_eq!(proof_info.vk.len(), data[1]);
        let proof_bytes: Vec<u8> = general_purpose::STANDARD_NO_PAD
            .decode(proof_info.proof)
            .expect("could not decode proof data from base64 string");
        let index_vk_bytes: Vec<u8> = general_purpose::STANDARD_NO_PAD
            .decode(proof_info.vk)
            .expect("could not decode index vk data from base64 string");
        assert_eq!(proof_bytes.len(), data[2]);
        assert_eq!(index_vk_bytes.len(), data[3]);
    }
}

#[test]
fn test_rust_verifiers() {
    println!("test_rust_verifiers");
    let program_id = on_deploy("test-data/sudoku.r1cs").unwrap();
    let witness_file = "test-data/sudoku.wtns";
    let proof_file = "proof.json";

    let proof_systems = [GevulotAlg::Marlin, GevulotAlg::Groth16];
    for ps in proof_systems {
        let prover_response = on_prove(ps, &program_id, &Some(witness_file), proof_file).unwrap();
        assert!(prover_response.header.success);
        assert!(prover_response.header.message.eq(PROVER_SUCCESS));

        // with correct public inputs
        let user_inputs = "test-data/sudoku_inputs.json";
        let verify_response = on_verify(proof_file, &Some(user_inputs)).unwrap();
        assert!(verify_response.header.success);
        assert!(verify_response.header.message.eq(VERIFICATION_SUCCESS));

        // with incorrect public inputs
        let user_inputs = "test-data/sudoku_wrong.json";
        let verify_response = on_verify(proof_file, &Some(user_inputs)).unwrap();
        assert!(!verify_response.header.success);
        assert!(verify_response.header.message.eq(VERIFICATION_FAIL));
    }
}

#[test]
fn test_rust_filecoin() {
    println!("test_rust_filecoin");

    // Window Proof-of-Spacetime
    let proof_inputs = &"window-post-inputs".to_owned();
    let proof_file = "window-proof.json";
    let prover_response =
        on_prove_filecoin(GevulotAlg::WindowPost, proof_inputs, proof_file).unwrap();
    println!("prover_response.header: {:?}", prover_response.header);
    assert!(prover_response.header.success);
    assert!(prover_response.header.message.eq(PROVER_SUCCESS));

    let user_inputs_file: Option<&str> = None;
    let verify_response = on_verify(proof_file, &user_inputs_file).unwrap();
    println!("verify_response.header: {:?}", verify_response.header);
    assert!(verify_response.header.success);
    assert!(verify_response.header.message.eq(VERIFICATION_SUCCESS));

    // Winning Proof-of-Spacetime
    let proof_inputs = &"winning-post-inputs".to_owned();
    let proof_file = "winning-proof.json";
    let prover_response =
        on_prove_filecoin(GevulotAlg::WinningPost, proof_inputs, proof_file).unwrap();
    println!("prover_response.header: {:?}", prover_response.header);
    assert!(prover_response.header.success);
    assert!(prover_response.header.message.eq(PROVER_SUCCESS));

    let user_inputs_file: Option<&str> = None;
    let verify_response = on_verify(proof_file, &user_inputs_file).unwrap();
    println!("verify_response.header: {:?}", verify_response.header);
    assert!(verify_response.header.success);
    assert!(verify_response.header.message.eq(VERIFICATION_SUCCESS));
}
