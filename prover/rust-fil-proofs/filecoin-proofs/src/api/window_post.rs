use crate::api::FilProofInfo;
use crate::{
    api::{
        as_safe_commitment, get_partitions_for_window_post, partition_vanilla_proofs,
        single_partition_vanilla_proofs,
    },
    caches::{get_post_params, get_post_verifying_key},
    parameters::window_post_setup_params,
    types::{ChallengeSeed, FallbackPoStSectorProof, PoStConfig, ProverId, SnarkProof},
    PartitionSnarkProof, PoStType,
};
use anyhow::{ensure, Error, Result};
use base64::{engine::general_purpose, Engine as _};
use filecoin_hashers::Hasher;
use log::info;
use storage_proofs_core::{
    compound_proof::{self, CompoundProof},
    merkle::MerkleTreeTrait,
    multi_proof::MultiProof,
};
use storage_proofs_post::fallback::{self, FallbackPoSt, FallbackPoStCompound, PublicSector};

/// Generates a Window proof-of-spacetime with provided vanilla proofs.
pub fn generate_window_post_with_vanilla<Tree: 'static + MerkleTreeTrait>(
    post_config: &PoStConfig,
    randomness: &ChallengeSeed,
    prover_id: ProverId,
    vanilla_proofs: Vec<FallbackPoStSectorProof<Tree>>,
) -> Result<SnarkProof> {
    info!("generate_window_post_with_vanilla:start");
    ensure!(
        post_config.typ == PoStType::Window,
        "invalid post config type"
    );

    let randomness_safe: <Tree::Hasher as Hasher>::Domain =
        as_safe_commitment(randomness, "randomness")?;
    let prover_id_safe: <Tree::Hasher as Hasher>::Domain =
        as_safe_commitment(&prover_id, "prover_id")?;

    let vanilla_params = window_post_setup_params(post_config);
    let partitions = get_partitions_for_window_post(vanilla_proofs.len(), post_config);

    let setup_params = compound_proof::SetupParams {
        vanilla_params,
        partitions,
        priority: post_config.priority,
    };

    let partitions = partitions.unwrap_or(1);

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

    info!("generate_window_post_with_vanilla:finish");

    proof.to_vec()
}

/// Generates a Winning proof-of-spacetime.
pub fn generate_window_post_gevulot<Tree: 'static + MerkleTreeTrait>(
    fil_proof_info: FilProofInfo,
) -> Result<FilProofInfo, Error> {
    info!("generate_winning_post_gevulot:start");

    // let fil_proof_info = read_from_file(inputs_file);
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

    println!("window post proof_str {}", proof_str);

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

/// Verifies a window proof-of-spacetime.
pub fn verify_window_post<Tree: 'static + MerkleTreeTrait>(
    fil_proof_info: FilProofInfo,
) -> Result<bool> {
    info!("verify_window_post:start");

    // let fil_proof_info = read_from_file("window-post-proof.json");
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
        let multi_proof = MultiProof::new_from_bytes(
            fil_proof_info.partitions,
            proof.as_slice(),
            &verifying_key,
        )?;

        FallbackPoStCompound::verify(
            &pub_params,
            &pub_inputs,
            &multi_proof,
            &fallback::ChallengeRequirements {
                minimum_challenge_count: post_config.challenge_count * post_config.sector_count,
            },
        )?
    };

    info!("is_valid: {}", is_valid);

    if !is_valid {
        return Ok(false);
    }

    info!("verify_window_post:finish");

    Ok(true)
}

/// Generates a Window proof-of-spacetime with provided vanilla proofs of a single partition.
pub fn generate_single_window_post_with_vanilla<Tree: 'static + MerkleTreeTrait>(
    post_config: &PoStConfig,
    randomness: &ChallengeSeed,
    prover_id: ProverId,
    vanilla_proofs: Vec<FallbackPoStSectorProof<Tree>>,
    partition_index: usize,
) -> Result<PartitionSnarkProof> {
    info!("generate_single_window_post_with_vanilla:start");
    ensure!(
        post_config.typ == PoStType::Window,
        "invalid post config type"
    );

    let randomness_safe: <Tree::Hasher as Hasher>::Domain =
        as_safe_commitment(randomness, "randomness")?;
    let prover_id_safe: <Tree::Hasher as Hasher>::Domain =
        as_safe_commitment(&prover_id, "prover_id")?;

    let vanilla_params = window_post_setup_params(post_config);
    let partitions = get_partitions_for_window_post(vanilla_proofs.len(), post_config);

    let setup_params = compound_proof::SetupParams {
        vanilla_params,
        partitions,
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
        k: Some(partition_index),
    };

    let partitioned_proofs = single_partition_vanilla_proofs(
        post_config,
        &pub_params.vanilla_params,
        &pub_inputs,
        &vanilla_proofs,
    )?;

    let proof = FallbackPoStCompound::prove_with_vanilla(
        &pub_params,
        &pub_inputs,
        vec![partitioned_proofs],
        &groth_params,
    )?;

    info!("generate_single_window_post_with_vanilla:finish");

    proof.to_vec().map(PartitionSnarkProof)
}
