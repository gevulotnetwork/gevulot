use ark_bn254::{Bn254, Fr};
use ark_ec::pairing::Pairing;
use ark_ff::PrimeField;
use ark_relations::r1cs::{
    ConstraintSynthesizer, ConstraintSystemRef, LinearCombination, SynthesisError, Variable,
};
use ark_serialize::{CanonicalDeserialize, CanonicalSerialize};
use ark_std::{
    rand::{RngCore, SeedableRng},
    test_rng,
    vec::Vec,
};

use base64::{engine::general_purpose, Engine as _};

use crate::{prepare_verifying_key, Groth16, PreparedVerifyingKey, Proof};
use ark_crypto_primitives::snark::{CircuitSpecificSetupSNARK, SNARK};
use ark_ff::{BigInteger, BigInteger256};
use gev_core::*;
use r1cs_file::{FieldElement, R1csFile};
use wtns_file::{FieldElement as WtnsFE, WtnsFile};

type Inputs<E> = Vec<<E as Pairing>::ScalarField>;

type ConstraintVec<E> = Vec<(usize, <E as Pairing>::ScalarField)>;
type Constraints<E> = (ConstraintVec<E>, ConstraintVec<E>, ConstraintVec<E>);

#[derive(Clone)]
struct Circuit<E: Pairing> {
    num_inputs: usize,
    num_outputs: usize,
    num_wires: usize,
    constraints: Vec<Constraints<E>>,
    witness: Vec<E::ScalarField>,
}

impl<E: Pairing> ConstraintSynthesizer<E::ScalarField> for Circuit<E> {
    fn generate_constraints(
        self,
        cs: ConstraintSystemRef<E::ScalarField>,
    ) -> Result<(), SynthesisError> {
        let w = &self.witness;

        let input_start = 1 + self.num_outputs;
        let input_end = input_start + self.num_inputs;

        // the inputs are after ONE (constant, index 0) and the outputs (if any)
        for i in 0..self.num_inputs {
            cs.new_input_variable(|| Ok(w[i + input_start]))?;
        }
        // If any outputs, they start at index 1
        for wval in w.iter().take(self.num_outputs + 1) {
            cs.new_witness_variable(|| Ok(*wval))?;
        }

        for i in 0..self.num_wires - 1 - self.num_outputs {
            cs.new_witness_variable(|| Ok(w[i + input_end]))?;
        }

        let make_index = |index| {
            if index < input_start {
                Variable::Witness(index)
            } else if index < input_end {
                Variable::Instance(index - input_start + 1)
            } else {
                Variable::Witness(index - self.num_inputs)
            }
        };
        let make_lc = |lc_data: &[(usize, E::ScalarField)]| {
            lc_data.iter().fold(
                LinearCombination::<E::ScalarField>::zero(),
                |lc: LinearCombination<E::ScalarField>, (index, coeff)| {
                    lc + (*coeff, make_index(*index))
                },
            )
        };

        for constraint in &self.constraints {
            cs.enforce_constraint(
                make_lc(&constraint.0),
                make_lc(&constraint.1),
                make_lc(&constraint.2),
            )?;
        }

        Ok(())
    }
}

fn create_circuit<E>(prover_inputs: ProverInputs) -> Result<Circuit<E>, GevulotError>
where
    E: Pairing,
{
    println!("  prover_inputs.r1cs len: {}", prover_inputs.r1cs.len());
    println!("  prover_inputs.wtns len: {}", prover_inputs.wtns.len());
    let r1cs_bytes: Vec<u8> = general_purpose::STANDARD_NO_PAD
        .decode(prover_inputs.r1cs)
        .expect("could not decode r1cs data from base64 string");
    let wtns_bytes: Vec<u8> = general_purpose::STANDARD_NO_PAD
        .decode(prover_inputs.wtns)
        .expect("could not decode wtns data from base64 string");

    println!("  r1cs_bytes len: {}", r1cs_bytes.len());
    println!("  wtns_bytes len: {}", wtns_bytes.len());
    let r1cs_file =
        R1csFile::<32>::read(r1cs_bytes.as_slice()).map_err(|_| GevulotError::R1csParseError)?;

    let wtns_file =
        WtnsFile::<32>::read(wtns_bytes.as_slice()).map_err(|_| GevulotError::WtnsParseError)?;

    let mut witness: Vec<E::ScalarField> = Vec::new();
    for fe in &wtns_file.witness.0 {
        let fr = wtns_field_element_to_fr::<E>(fe);
        witness.push(fr);
    }

    println!("  create_circuit");
    println!("  witness len: {}", witness.len());
    println!("  constraints len: {}", r1cs_file.constraints.0.len());
    println!("  header: {:?}", r1cs_file.header);

    let mut constraints: Vec<Constraints<E>> = Vec::with_capacity(r1cs_file.constraints.0.len());
    println!("  alloced constraints: {:?}", constraints);

    let mut nn = 0 as usize;
    for c in &r1cs_file.constraints.0 {
        let mut c0: Vec<(usize, E::ScalarField)> = Vec::new();
        for n in 0..c.0.len() {
            let i = c.0[n].1 as usize;
            let fr: <E as Pairing>::ScalarField = r1cs_field_element_to_fr::<E>(&c.0[n].0);
            c0.push((i, fr));
        }
        let mut c1: Vec<(usize, E::ScalarField)> = Vec::new();
        for n in 0..c.1.len() {
            let i = c.1[n].1 as usize;
            let fr = r1cs_field_element_to_fr::<E>(&c.1[n].0);
            c1.push((i, fr));
        }
        let mut c2: Vec<(usize, E::ScalarField)> = Vec::new();
        for n in 0..c.2.len() {
            let i = c.2[n].1 as usize;
            let fr = r1cs_field_element_to_fr::<E>(&c.2[n].0);
            c2.push((i, fr));
        }

        nn += 1;

        if nn % 4000 == 0 {
            println!(" constaints: {}", nn);
        }

        let constraint = (c0, c1, c2);
        constraints.push(constraint);
    }

    println!("  constraints.len(): {:?}", constraints.len());

    let num_inputs = r1cs_file.header.n_pub_in as usize;
    let num_wires = r1cs_file.header.n_wires as usize - num_inputs;
    let num_outputs = r1cs_file.header.n_pub_out as usize;
    println!("  num_inputs: {:?}", num_inputs);
    println!("  num_outputs: {:?}", num_outputs);
    println!("  num_wires: {:?}", num_wires);

    Ok(Circuit {
        num_inputs,
        num_outputs,
        num_wires,
        constraints,
        witness,
    })
}

/// This is our Rust prover function
pub fn do_prove<E>(inputs: ProverInputs) -> Result<ProverResponse, GevulotError>
where
    E: Pairing,
{
    println!("groth16::do_prove");
    let start = get_millis();

    let circ = create_circuit::<E>(inputs)?;
    let mut rng = ark_std::rand::rngs::StdRng::seed_from_u64(test_rng().next_u64());

    let c = circ.clone();
    let (pk, vk) = Groth16::<E>::setup(c, &mut rng).map_err(|_| GevulotError::Groth16SetupError)?;
    let pvk = prepare_verifying_key::<E>(&vk);

    let proof = Groth16::<E>::prove(&pk, circ, &mut rng).unwrap();
    println!("  proof {:?}", &proof);

    let mut proof_bytes = Vec::new();
    proof
        .serialize_uncompressed(&mut proof_bytes)
        .map_err(|_| GevulotError::CanonicalSerializeError)?;
    let mut pvk_bytes = Vec::new();

    pvk.serialize_uncompressed(&mut pvk_bytes)
        .map_err(|_| GevulotError::CanonicalSerializeError)?;

    let proof_encoded = general_purpose::STANDARD_NO_PAD.encode(&proof_bytes);
    let pvk_encoded = general_purpose::STANDARD_NO_PAD.encode(&pvk_bytes);
    println!("  proof {:?}", &proof);
    println!("  proof_encoded len {}", proof_encoded.len());
    println!("  pvk_encoded len {}", pvk_encoded.len());

    let proof_info = ProofInfo {
        algorithm: "groth16".to_owned(),
        proof: proof_encoded,
        vk: pvk_encoded,
    };
    println!("  do_prove took {} ms", get_millis() - start);
    Ok(ProverResponse {
        header: ResponseHeader {
            success: true,
            message: PROVER_SUCCESS.to_string(),
        },
        proof_info: Some(proof_info),
    })
}

/// Groth16 Rust verification
pub fn do_verify<E: Pairing>(
    proof_bytes: &Vec<u8>,
    index_vk_bytes: &Vec<u8>,
    inputs: &Inputs<E>,
) -> Result<VerifyResponse, GevulotError> {
    println!("do_verify");
    let start = get_millis();

    let proof = Proof::<E>::deserialize_uncompressed(proof_bytes.as_slice())
        .map_err(|_| GevulotError::CanonicalSerializeError)?;

    let pvk = PreparedVerifyingKey::<E>::deserialize_uncompressed(index_vk_bytes.as_slice())
        .map_err(|_| GevulotError::CanonicalSerializeError)?;

    let success = Groth16::<E>::verify_with_processed_vk(&pvk, inputs.as_slice(), &proof)
        .map_err(|_| GevulotError::Groth16VerifyError)?;

    println!("  do_verify: success = {}", success);
    println!("  do_verify took {} ms", get_millis() - start);

    let message = if success {
        VERIFICATION_SUCCESS.to_string()
    } else {
        VERIFICATION_FAIL.to_string()
    };
    Ok(VerifyResponse {
        header: ResponseHeader { success, message },
    })
}

fn bigint256_to_fr<E: Pairing>(b: &BigInteger256) -> E::ScalarField {
    E::ScalarField::from_le_bytes_mod_order(b.to_owned().to_bytes_le().as_slice())
}

fn r1cs_field_element_to_fr<E: Pairing>(fe: &FieldElement<32>) -> E::ScalarField {
    E::ScalarField::from_le_bytes_mod_order(fe.to_owned().to_vec().as_slice())
}

fn wtns_field_element_to_fr<E: Pairing>(fe: &WtnsFE<32>) -> E::ScalarField {
    E::ScalarField::from_le_bytes_mod_order(fe.to_owned().to_vec().as_slice())
}

/// exported for Rust calling
pub fn verify<E: Pairing>(verify_inputs: VerifyInputs) -> Result<VerifyResponse, GevulotError> {
    println!("inputs {:?}", verify_inputs.inputs);
    let proof: Vec<u8> = general_purpose::STANDARD_NO_PAD
        .decode(verify_inputs.proof_info.proof)
        .map_err(|_| GevulotError::Base64DecodeError)?;
    let vk: Vec<u8> = general_purpose::STANDARD_NO_PAD
        .decode(verify_inputs.proof_info.vk)
        .map_err(|_| GevulotError::Base64DecodeError)?;
    let inputs = verify_inputs.inputs;

    println!("  proof len {}", proof.len());
    println!("  vk len {}", vk.len());
    println!("  inputs len {}", inputs.len());
    let mut fr_inputs: Vec<Fr> = Vec::new();
    for input in inputs {
        let fr = bigint256_to_fr::<Bn254>(&BigInteger256::new([input, 0, 0, 0]));
        fr_inputs.push(fr);
    }
    do_verify::<Bn254>(&proof, &vk, &fr_inputs)
}

#[cfg(test)]
mod test {
    use super::*;

    fn circuit_test<E: Pairing>() {
        let data = std::fs::read("../test-data/sudoku.r1cs")
            .expect("could not read ../test-data/sudoku.r1cs");
        let r1cs = general_purpose::STANDARD_NO_PAD.encode(&data);
        let data = std::fs::read("../test-data/sudoku.wtns")
            .expect("could not read ../test-data/sudoku.wtns");
        let wtns = general_purpose::STANDARD_NO_PAD.encode(&data);

        let prover_inputs = ProverInputs { r1cs, wtns };

        let prove_response = do_prove::<E>(prover_inputs).unwrap();
        let proof_info = prove_response
            .proof_info
            .expect("proof_info is not present");
        let proof_bytes: Vec<u8> = general_purpose::STANDARD_NO_PAD
            .decode(proof_info.proof)
            .expect("could not proof r1cs data from base64 string");

        let pvk_bytes: Vec<u8> = general_purpose::STANDARD_NO_PAD
            .decode(proof_info.vk)
            .expect("could not decode pvk data from base64 string");

        let mut correct: Vec<E::ScalarField> = Vec::new();
        for r in TEST_SUDOKU_CORRECT {
            let fr = bigint256_to_fr::<E>(&BigInteger256::new([r, 0, 0, 0]));
            correct.push(fr);
        }

        let mut wrong: Vec<E::ScalarField> = Vec::new();
        for r in TEST_SUDOKU_WRONG {
            let fr = bigint256_to_fr::<E>(&BigInteger256::new([r, 0, 0, 0]));
            wrong.push(fr);
        }

        let verify_response = do_verify::<E>(&proof_bytes, &pvk_bytes, &correct).unwrap();
        assert!(verify_response.header.success);
        assert!(verify_response.header.message.eq(VERIFICATION_SUCCESS));

        let verify_response = do_verify::<E>(&proof_bytes, &pvk_bytes, &wrong).unwrap();
        assert!(!verify_response.header.success);
        assert!(verify_response.header.message.eq(VERIFICATION_FAIL));
    }

    fn bad_verification_input<E: Pairing>() {
        let data = std::fs::read("../test-data/sudoku.r1cs")
            .expect("could not read ../test-data/sudoku.r1cs");
        let r1cs = general_purpose::STANDARD_NO_PAD.encode(&data);
        let data = std::fs::read("../test-data/sudoku.wtns")
            .expect("could not read ../test-data/sudoku.wtns");
        let wtns = general_purpose::STANDARD_NO_PAD.encode(&data);

        let prover_inputs = ProverInputs { r1cs, wtns };

        let prove_response = do_prove::<E>(prover_inputs).unwrap();
        let proof_info = prove_response
            .proof_info
            .expect("proof_info is not present");
        let proof_bytes: Vec<u8> = general_purpose::STANDARD_NO_PAD
            .decode(proof_info.proof)
            .expect("could not decode proof data from base64 string");

        let mut bad_proof_bytes = proof_bytes.clone();
        bad_proof_bytes[0] = 252;
        bad_proof_bytes[1] = 253;
        bad_proof_bytes[2] = 254;
        bad_proof_bytes[3] = 255;

        let pvk_bytes: Vec<u8> = general_purpose::STANDARD_NO_PAD
            .decode(proof_info.vk)
            .expect("could not decode pvk data from base64 string");
        let mut bad_pvk_bytes = pvk_bytes.clone();
        bad_pvk_bytes[0] = 252;
        bad_pvk_bytes[1] = 253;
        bad_pvk_bytes[2] = 254;
        bad_pvk_bytes[3] = 255;

        let mut inputs: Vec<E::ScalarField> = Vec::new();
        for r in TEST_SUDOKU_CORRECT {
            let fr = bigint256_to_fr::<E>(&BigInteger256::new([r, 0, 0, 0]));
            inputs.push(fr);
        }

        let result = do_verify::<E>(&bad_proof_bytes, &pvk_bytes, &inputs);
        assert!(matches!(result, Err(GevulotError::CanonicalSerializeError)));

        let result = do_verify::<E>(&proof_bytes, &bad_pvk_bytes, &inputs);
        assert!(matches!(result, Err(GevulotError::CanonicalSerializeError)));
    }

    #[test]
    fn test_circom() {
        circuit_test::<Bn254>();
    }

    #[test]
    fn test_bad_prover_input() {
        // swap r1cs with wtns
        let data = std::fs::read("../test-data/sudoku.r1cs")
            .expect("could not read ../test-data/sudoku.r1cs");
        let r1cs = general_purpose::STANDARD_NO_PAD.encode(&data);
        let data = std::fs::read("../test-data/sudoku.wtns")
            .expect("could not read ../test-data/sudoku.wtns");
        let wtns = general_purpose::STANDARD_NO_PAD.encode(&data);

        // send bad r1cs data
        let prover_inputs = ProverInputs {
            r1cs: wtns.clone(),
            wtns: wtns.clone(),
        };
        let result = do_prove::<Bn254>(prover_inputs).map_err(|e| e);
        println!("bad result: {:?}", result);
        assert!(matches!(result, Err(GevulotError::R1csParseError)));

        // send bad wtns data
        let prover_inputs = ProverInputs {
            r1cs: r1cs.clone(),
            wtns: r1cs.clone(),
        };
        let result = do_prove::<Bn254>(prover_inputs);
        assert!(matches!(result, Err(GevulotError::WtnsParseError)));
    }

    #[test]
    fn test_bad_verification_input() {
        bad_verification_input::<Bn254>();
    }

    #[test]
    #[ignore]
    fn test_ed25519_prover() {
        println!("test_ed25519_prover");
        let start = get_millis();
        let data = std::fs::read("../test-data/ed25519.r1cs")
            .expect("could not read ../test-data/ed25519.r1cs");
        let r1cs = general_purpose::STANDARD_NO_PAD.encode(&data);
        let data = std::fs::read("../test-data/ed25519.wtns")
            .expect("could not read ../test-data/ed25519.wtns");
        let wtns = general_purpose::STANDARD_NO_PAD.encode(&data);

        // send bad r1cs data
        let prover_inputs = ProverInputs { r1cs, wtns };

        let prover_response = do_prove::<Bn254>(prover_inputs).unwrap();
        let proof_info = prover_response
            .proof_info
            .expect("proof_info is not present");
        let proof_bytes: Vec<u8> = general_purpose::STANDARD_NO_PAD
            .decode(proof_info.proof)
            .expect("could not decode proof data from base64 string");
        let index_vk_bytes: Vec<u8> = general_purpose::STANDARD_NO_PAD
            .decode(proof_info.vk)
            .expect("could not decode index vk data from base64 string");

        println!("  proof len {}", proof_bytes.len());
        println!("  vk len {}", index_vk_bytes.len());
        let fr_inputs: Vec<Fr> = Vec::new();

        let verify_response = do_verify::<Bn254>(&proof_bytes, &index_vk_bytes, &fr_inputs);
        println!("verify_response: {:?}", verify_response);
        println!("  test_ed25519_prover took {} ms", get_millis() - start);
    }
}
