use crate::api::FilProofInfo;
use anyhow::{ensure, Result};
use filecoin_hashers::Hasher;
use log::info;

use anyhow::Error;
use base64::{engine::general_purpose, Engine as _};
use storage_proofs_core::{
    compound_proof::{self, CompoundProof},
    merkle::MerkleTreeTrait,
    multi_proof::MultiProof,
};
use storage_proofs_post::fallback::{
    self, generate_sector_challenges, FallbackPoSt, FallbackPoStCompound, PublicSector,
};

use crate::{
    api::{as_safe_commitment, partition_vanilla_proofs},
    caches::{get_post_params, get_post_verifying_key},
    parameters::winning_post_setup_params,
    types::{ChallengeSeed, Commitment, FallbackPoStSectorProof, PoStConfig, ProverId, SnarkProof},
    PoStType,
};

/// Generates a Winning proof-of-spacetime with provided vanilla proofs.
pub fn generate_winning_post_with_vanilla<Tree: 'static + MerkleTreeTrait>(
    post_config: &PoStConfig,
    randomness: &ChallengeSeed,
    prover_id: ProverId,
    vanilla_proofs: Vec<FallbackPoStSectorProof<Tree>>,
) -> Result<SnarkProof> {
    info!("generate_winning_post_with_vanilla:start");
    ensure!(
        post_config.typ == PoStType::Winning,
        "invalid post config type"
    );

    ensure!(
        vanilla_proofs.len() == post_config.sector_count,
        "invalid amount of vanilla proofs"
    );

    let randomness_safe: <Tree::Hasher as Hasher>::Domain =
        as_safe_commitment(randomness, "randomness")?;
    let prover_id_safe: <Tree::Hasher as Hasher>::Domain =
        as_safe_commitment(&prover_id, "prover_id")?;

    let vanilla_params = winning_post_setup_params(post_config)?;

    let setup_params = compound_proof::SetupParams {
        vanilla_params,
        partitions: None,
        priority: post_config.priority,
    };
    let pub_params: compound_proof::PublicParams<'_, FallbackPoSt<'_, Tree>> =
        FallbackPoStCompound::setup(&setup_params)?;
    let groth_params = get_post_params::<Tree>(post_config)?;

    let mut pub_sectors = Vec::with_capacity(vanilla_proofs.len());
    for vanilla_proof in &vanilla_proofs {
        pub_sectors.push(PublicSector {
            id: vanilla_proof.sector_id,
            comm_r: vanilla_proof.comm_r,
        });
    }

    let pub_inputs = fallback::PublicInputs {
        randomness: randomness_safe,
        prover_id: prover_id_safe,
        sectors: pub_sectors,
        k: None,
    };

    let partitions = pub_params.partitions.unwrap_or(1);
    let partitioned_proofs = partition_vanilla_proofs(
        post_config,
        &pub_params.vanilla_params,
        &pub_inputs,
        partitions,
        &vanilla_proofs,
    )?;

    let proof = FallbackPoStCompound::prove_with_vanilla(
        &pub_params,
        &pub_inputs,
        partitioned_proofs,
        &groth_params,
    )?;
    let proof = proof.to_vec()?;

    info!("generate_winning_post_with_vanilla:finish");

    Ok(proof)
}

/// Generates a Winning proof-of-spacetime.
pub fn generate_winning_post_gevulot<Tree: 'static + MerkleTreeTrait>(
    fil_proof_info: FilProofInfo,
) -> Result<FilProofInfo, Error> {
    info!("generate_winning_post_gevulot:start");

    let post_config: &PoStConfig = &serde_json::from_str(&fil_proof_info.post_config).unwrap();

    let groth_params = get_post_params::<Tree>(post_config).unwrap();

    let pub_params: compound_proof::PublicParams<'_, FallbackPoSt<'_, Tree>> =
        serde_json::from_str(&fil_proof_info.pub_params).unwrap();

    let pub_inputs: fallback::PublicInputs<<<Tree as MerkleTreeTrait>::Hasher as Hasher>::Domain> =
        serde_json::from_str(&fil_proof_info.pub_inputs).unwrap();

    let proof = FallbackPoStCompound::<Tree>::prove_with_vanilla_inputs(
        &pub_params,
        &pub_inputs,
        &fil_proof_info.vanilla_proofs,
        &groth_params,
    )
    .unwrap();
    let proof = proof.to_vec().unwrap();
    let proof_str = general_purpose::STANDARD_NO_PAD.encode(&proof);
    println!("winning post proof_str {}", proof_str);

    let fil_proof_export = FilProofInfo {
        post_config: fil_proof_info.post_config,
        pub_inputs: fil_proof_info.pub_inputs,
        pub_params: fil_proof_info.pub_params,
        vanilla_proofs: fil_proof_info.vanilla_proofs,
        proof: proof_str,
        partitions: fil_proof_info.partitions,
    };

    info!("generate_winning_post_gevulot:finish");

    Ok(fil_proof_export)
}

/// Given some randomness and the length of available sectors, generates the challenged sector.
///
/// The returned values are indices in the range of `0..sector_set_size`, requiring the caller
/// to match the index to the correct sector.
pub fn generate_winning_post_sector_challenge<Tree: MerkleTreeTrait>(
    post_config: &PoStConfig,
    randomness: &ChallengeSeed,
    sector_set_size: u64,
    prover_id: Commitment,
) -> Result<Vec<u64>> {
    info!("generate_winning_post_sector_challenge:start");
    ensure!(sector_set_size != 0, "empty sector set is invalid");
    ensure!(
        post_config.typ == PoStType::Winning,
        "invalid post config type"
    );

    let prover_id_safe: <Tree::Hasher as Hasher>::Domain =
        as_safe_commitment(&prover_id, "prover_id")?;

    let randomness_safe: <Tree::Hasher as Hasher>::Domain =
        as_safe_commitment(randomness, "randomness")?;
    let result = generate_sector_challenges(
        randomness_safe,
        post_config.sector_count,
        sector_set_size,
        prover_id_safe,
    );

    info!("generate_winning_post_sector_challenge:finish");

    result
}

/// Verifies a winning proof-of-spacetime.
///
/// The provided `replicas` must be the same ones as passed to `generate_winning_post`, and be based on
/// the indices generated by `generate_winning_post_sector_challenge`. It is the responsibility of the
/// caller to ensure this.
pub fn verify_winning_post<Tree: 'static + MerkleTreeTrait>(
    fil_proof_info: FilProofInfo,
) -> Result<bool> {
    info!("verify_winning_post:start");

    // let fil_proof_info = read_from_file(proof_file);
    let post_config: &PoStConfig = &serde_json::from_str(&fil_proof_info.post_config).unwrap();
    let pub_params: compound_proof::PublicParams<'_, FallbackPoSt<'_, Tree>> =
        serde_json::from_str(&fil_proof_info.pub_params).unwrap();
    let pub_inputs: fallback::PublicInputs<<<Tree as MerkleTreeTrait>::Hasher as Hasher>::Domain> =
        serde_json::from_str(&fil_proof_info.pub_inputs).unwrap();
    let proof = general_purpose::STANDARD_NO_PAD
        .decode(fil_proof_info.proof)
        .expect("could not decode proof bytes from base64 string");

    let is_valid = {
        let verifying_key = get_post_verifying_key::<Tree>(post_config)?;

        let single_proof = MultiProof::new_from_reader(None, proof.as_slice(), &verifying_key)?;
        if single_proof.len() != 1 {
            return Ok(false);
        }

        FallbackPoStCompound::verify(
            &pub_params,
            &pub_inputs,
            &single_proof,
            &fallback::ChallengeRequirements {
                minimum_challenge_count: post_config.challenge_count * post_config.sector_count,
            },
        )?
    };

    info!("is_valid: {}", is_valid);

    if !is_valid {
        return Ok(false);
    }

    info!("verify_winning_post:finish");

    Ok(true)
}
