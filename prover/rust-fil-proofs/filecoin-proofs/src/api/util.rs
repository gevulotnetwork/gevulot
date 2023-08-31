use std::mem::size_of;

use crate::types::{Commitment, SectorSize};
use anyhow::{Context, Result};
use blstrs::Scalar as Fr;
use filecoin_hashers::{Domain, Hasher};
use fr32::{bytes_into_fr, fr_into_bytes};
use merkletree::merkle::{get_merkle_tree_leafs, get_merkle_tree_len};
use serde::{Deserialize, Serialize};
use storage_proofs_core::merkle::{get_base_tree_count, MerkleTreeTrait};
use typenum::Unsigned;

#[derive(Default, Clone, Serialize, Deserialize, Debug)]
pub struct FilProofInfo {
    pub post_config: String,
    pub pub_inputs: String,
    pub pub_params: String,
    pub vanilla_proofs: String,
    pub proof: String,
    pub partitions: Option<usize>,
}

// pub fn write_to_file(deployment_file: impl AsRef<Path>, fil_proof_info: FilProofInfo) {
//     println!("write_to_file: {:?}", &deployment_file.as_ref().display());
//     let deployment = json!(fil_proof_info).to_string();
//     println!("  deployment len: {}", deployment.len());
//     std::fs::write(deployment_file, deployment).unwrap();
// }

// pub fn read_from_file(deployment_file: impl AsRef<Path>) -> FilProofInfo {
//     println!("read_from_file: {:?}", &deployment_file.as_ref().display());
//     let proofstr = std::fs::read_to_string(deployment_file).unwrap();
//     let fil_proof_info: FilProofInfo = serde_json::from_str(&proofstr).unwrap();
//     fil_proof_info
// }

pub fn as_safe_commitment<H: Domain, T: AsRef<str>>(
    comm: &[u8; 32],
    commitment_name: T,
) -> Result<H> {
    bytes_into_fr(comm)
        .map(Into::into)
        .with_context(|| format!("Invalid commitment ({})", commitment_name.as_ref(),))
}

pub fn commitment_from_fr(fr: Fr) -> Commitment {
    let mut commitment = [0; 32];
    for (i, b) in fr_into_bytes(&fr).iter().enumerate() {
        commitment[i] = *b;
    }
    commitment
}

pub fn get_base_tree_size<Tree: MerkleTreeTrait>(sector_size: SectorSize) -> Result<usize> {
    let base_tree_leaves = u64::from(sector_size) as usize
        / size_of::<<Tree::Hasher as Hasher>::Domain>()
        / get_base_tree_count::<Tree>();

    get_merkle_tree_len(base_tree_leaves, Tree::Arity::to_usize())
}

pub fn get_base_tree_leafs<Tree: MerkleTreeTrait>(base_tree_size: usize) -> Result<usize> {
    get_merkle_tree_leafs(base_tree_size, Tree::Arity::to_usize())
}
