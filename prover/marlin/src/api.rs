use crate::IndexVerifierKey;
use crate::Marlin;
use crate::Proof;
use ark_bn254::{Bn254, Fr};
use ark_ff::{Field, FromBytes};
use ark_poly::univariate::DensePolynomial;
use ark_poly_commit::marlin_pc::MarlinKZG10;
use ark_relations::r1cs::{
    ConstraintSynthesizer, ConstraintSystemRef, LinearCombination, SynthesisError, Variable,
};
use ark_serialize::{CanonicalDeserialize, CanonicalSerialize};
use ark_std::{io::Read, vec::Vec};
use base64::{engine::general_purpose, Engine as _};
use blake2::Blake2s;
use r1cs_file::{FieldElement, R1csFile};
use wtns_file::{FieldElement as WtnsFE, WtnsFile};
type MultiPC = MarlinKZG10<Bn254, DensePolynomial<Fr>>;
type MarlinInst = Marlin<Fr, MultiPC, Blake2s>;
type Inputs = Vec<Fr>;
type ConstraintVec = Vec<(usize, Fr)>;
type Constraints = (ConstraintVec, ConstraintVec, ConstraintVec);
use ark_ff::{BigInteger, BigInteger256};
use gev_core::*;

#[derive(Clone)]
struct Circuit<F: Field> {
    pub(crate) num_inputs: usize,
    pub(crate) num_outputs: usize,
    pub(crate) num_wires: usize,
    pub(crate) constraints: Vec<Constraints>,
    pub(crate) witness: Vec<F>,
}

impl ConstraintSynthesizer<Fr> for Circuit<Fr> {
    fn generate_constraints(self, cs: ConstraintSystemRef<Fr>) -> Result<(), SynthesisError> {
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
        // the rest of the witness variables
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
        let make_lc = |lc_data: &[(usize, Fr)]| {
            lc_data.iter().fold(
                LinearCombination::<Fr>::zero(),
                |lc: LinearCombination<Fr>, (index, coeff)| lc + (*coeff, make_index(*index)),
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

fn create_circuit(prover_inputs: ProverInputs) -> Result<Circuit<Fr>, GevulotError> {
    let r1cs_bytes: Vec<u8> = general_purpose::STANDARD_NO_PAD
        .decode(prover_inputs.r1cs)
        .expect("could not decode r1cs data from base64 string");
    let wtns_bytes: Vec<u8> = general_purpose::STANDARD_NO_PAD
        .decode(prover_inputs.wtns)
        .expect("could not decode wtns data from base64 string");

    let r1cs_file =
        R1csFile::<32>::read(r1cs_bytes.as_slice()).map_err(|_| GevulotError::R1csParseError)?;

    let wtns_file =
        WtnsFile::<32>::read(wtns_bytes.as_slice()).map_err(|_| GevulotError::R1csParseError)?;

    let mut witness: Vec<Fr> = Vec::new();
    for fe in &wtns_file.witness.0 {
        let fr = wtns_field_element_to_fr(fe);
        witness.push(fr);
    }

    let mut constraints: Vec<Constraints> = Vec::new();

    println!(" create_circuit");
    println!("  witness len: {}", witness.len());
    println!("  constraints len: {}", r1cs_file.constraints.0.len());
    for c in &r1cs_file.constraints.0 {
        let mut c0: Vec<(usize, Fr)> = Vec::new();
        for n in 0..c.0.len() {
            let i = c.0[n].1 as usize;
            let fr = r1cs_field_element_to_fr(&c.0[n].0);
            c0.push((i, fr));
        }
        let mut c1: Vec<(usize, Fr)> = Vec::new();
        for n in 0..c.1.len() {
            let i = c.1[n].1 as usize;
            let fr = r1cs_field_element_to_fr(&c.1[n].0);
            c1.push((i, fr));
        }
        let mut c2: Vec<(usize, Fr)> = Vec::new();
        for n in 0..c.2.len() {
            let i = c.2[n].1 as usize;
            let fr = r1cs_field_element_to_fr(&c.2[n].0);
            c2.push((i, fr));
        }

        let constraint: Constraints = (c0, c1, c2);
        constraints.push(constraint);
    }

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
pub fn do_prove(inputs: ProverInputs) -> Result<ProverResponse, GevulotError> {
    let rng = &mut ark_std::test_rng();
    // TODO: get num constraints
    println!("marlin::do_prove");
    let start = get_millis();

    let circ = create_circuit(inputs)?;

    // let num_constraints = circ.constraints.len();
    // let num_variables = circ.num_wires;
    // let num_non_zero = circ.num_wires * 2;
    let num_constraints = 10000;
    let num_variables = 10000;
    let num_non_zero = 10000;

    let universal_srs =
        MarlinInst::universal_setup(num_constraints, num_variables, num_non_zero, rng).unwrap();

    let (index_pk, index_vk) = MarlinInst::index(&universal_srs, circ.clone()).unwrap();

    let proof = match MarlinInst::prove(&index_pk, circ, rng) {
        Ok(pi) => pi,
        Err(error) => {
            panic!("MarlinInst::prove error: {:?}", error);
        }
    };

    let mut proof_bytes = Vec::new();
    proof
        .serialize_uncompressed(&mut proof_bytes)
        .map_err(|_| GevulotError::CanonicalSerializeError)?;

    let mut index_vk_bytes = Vec::new();
    index_vk
        .serialize_uncompressed(&mut index_vk_bytes)
        .map_err(|_| GevulotError::CanonicalSerializeError)?;

    println!("  proof_bytes len {:?}", proof_bytes.len());
    println!("  index_vk_bytes len {:?}", index_vk_bytes.len());

    let proof_encoded = general_purpose::STANDARD_NO_PAD.encode(&proof_bytes);
    let index_vk_encoded = general_purpose::STANDARD_NO_PAD.encode(&index_vk_bytes);
    let proof_info = ProofInfo {
        algorithm: "marlin".to_owned(),
        proof: proof_encoded,
        vk: index_vk_encoded,
    };

    println!("  do_prove took {} ms", get_millis() - start);
    Ok(ProverResponse {
        header: ResponseHeader {
            success: true,
            message: "Prover succeeded".to_owned(),
        },
        proof_info: Some(proof_info),
    })
}

pub fn do_verify(
    proof_bytes: &Vec<u8>,
    index_vk_bytes: &Vec<u8>,
    inputs: &Inputs,
) -> Result<VerifyResponse, GevulotError> {
    println!("marlin::do_verify");
    let rng = &mut ark_std::test_rng();
    let start = get_millis();

    type ProofType = Proof<Fr, MultiPC>;
    let proof = ProofType::deserialize_uncompressed(proof_bytes.as_slice())
        .map_err(|_| GevulotError::CanonicalSerializeError)?;

    type IndexVkType = IndexVerifierKey<Fr, MultiPC>;
    let index_vk = IndexVkType::deserialize_uncompressed(index_vk_bytes.as_slice())
        .map_err(|_| GevulotError::CanonicalSerializeError)?;

    let success = MarlinInst::verify(&index_vk, inputs, &proof, rng)
        .map_err(|_| GevulotError::MarlinVerifyError)?;

    println!("  do_verify: success = {}", success);

    let message = if success {
        VERIFICATION_SUCCESS.to_string()
    } else {
        VERIFICATION_FAIL.to_string()
    };
    println!("  do_verify took {} ms", get_millis() - start);

    Ok(VerifyResponse {
        header: ResponseHeader { success, message },
    })
}

fn read_field_element<R: Read>(reader: R) -> Fr {
    Fr::read(reader).expect("could not read field element")
}

fn bigint256_to_fr(b: &BigInteger256) -> Fr {
    read_field_element(b.to_owned().to_bytes_le().as_slice())
}
fn r1cs_field_element_to_fr(fe: &FieldElement<32>) -> Fr {
    read_field_element(fe.to_owned().to_vec().as_slice())
}

fn wtns_field_element_to_fr(fe: &WtnsFE<32>) -> Fr {
    read_field_element(fe.to_owned().to_vec().as_slice())
}

// fn unpack_and_prove(jproverinputs: String) -> Result<ProverResponse, GevulotError> {
//     let prover_inputs: ProverInputs =
//         serde_json::from_str(&jproverinputs).map_err(|_| GevulotError::JsonDeserializeError)?;
//     println!("  r1cs len {:?}", prover_inputs.r1cs.len());
//     println!("  wtns len {:?}", prover_inputs.wtns.len());

//     do_prove(prover_inputs)
// }

// fn unpack_and_verify(jverifyinputs: String) -> Result<VerifyResponse, GevulotError> {
//     let verify_inputs: VerifyInputs =
//         serde_json::from_str(&jverifyinputs).map_err(|_| GevulotError::JsonDeserializeError)?;
//     verify(verify_inputs)
// }

pub fn verify(verify_inputs: VerifyInputs) -> Result<VerifyResponse, GevulotError> {
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
        let fr = bigint256_to_fr(&BigInteger256::new([input, 0, 0, 0]));
        fr_inputs.push(fr);
    }
    do_verify(&proof, &vk, &fr_inputs)
}

#[test]
fn test_circom() {
    let data =
        std::fs::read("../test-data/sudoku.r1cs").expect("could not read ../test-data/sudoku.r1cs");
    let r1cs = general_purpose::STANDARD_NO_PAD.encode(&data);
    let data =
        std::fs::read("../test-data/sudoku.wtns").expect("could not read ../test-data/sudoku.wtns");
    let wtns = general_purpose::STANDARD_NO_PAD.encode(&data);

    let prover_inputs = ProverInputs { r1cs, wtns };

    let prove_response = do_prove(prover_inputs).unwrap();
    let proof_info = prove_response
        .proof_info
        .expect("proof_info is not present");
    let proof_bytes: Vec<u8> = general_purpose::STANDARD_NO_PAD
        .decode(proof_info.proof)
        .expect("could not decode proof data from base64 string");

    let vk_bytes: Vec<u8> = general_purpose::STANDARD_NO_PAD
        .decode(proof_info.vk)
        .expect("could not decode vk data from base64 string");

    type ProofType = Proof<Fr, MultiPC>;
    assert!(ProofType::deserialize_uncompressed(proof_bytes.as_slice()).is_ok());
    type IndexVkType = IndexVerifierKey<Fr, MultiPC>;
    assert!(IndexVkType::deserialize_uncompressed(vk_bytes.as_slice()).is_ok());

    let mut correct: Vec<Fr> = Vec::new();
    for r in TEST_SUDOKU_CORRECT {
        let fr = bigint256_to_fr(&BigInteger256([r, 0, 0, 0]));
        correct.push(fr);
    }

    let mut wrong: Vec<Fr> = Vec::new();
    for r in TEST_SUDOKU_WRONG {
        let fr = bigint256_to_fr(&BigInteger256([r, 0, 0, 0]));
        wrong.push(fr);
    }

    let verify_response = do_verify(&proof_bytes, &vk_bytes, &correct);
    println!("correct data: {:?}", verify_response);

    let verify_response = do_verify(&proof_bytes, &vk_bytes, &wrong);
    println!("wrong data: {:?}", verify_response);
}

#[test]
fn test_bad_prover_input() {
    // swap r1cs with wtns
    let data = std::fs::read("../test-data/sudoku.r1cs")
        .expect("could not read file ../test-data/sudoku.r1cs");
    let r1cs = general_purpose::STANDARD_NO_PAD.encode(&data);
    let data = std::fs::read("../test-data/sudoku.wtns")
        .expect("could not read file ../test-data/sudoku.wtns");
    let wtns = general_purpose::STANDARD_NO_PAD.encode(&data);

    // send bad r1cs data
    let prover_inputs = ProverInputs {
        r1cs: wtns.clone(),
        wtns: wtns.clone(),
    };

    let result = do_prove(prover_inputs);
    assert!(matches!(result, Err(GevulotError::R1csParseError)));

    // send bad wtns data
    let prover_inputs = ProverInputs {
        r1cs: r1cs.clone(),
        wtns: r1cs.clone(),
    };

    let result = do_prove(prover_inputs);
    assert!(matches!(result, Err(GevulotError::R1csParseError)));
}

#[test]
fn test_bad_verification_input() {
    let data = std::fs::read("../test-data/sudoku.r1cs")
        .expect("could not read file ../test-data/sudoku.r1cs");
    let r1cs = general_purpose::STANDARD_NO_PAD.encode(&data);
    let data = std::fs::read("../test-data/sudoku.wtns")
        .expect("could not read file ../test-data/sudoku.wtns");
    let wtns = general_purpose::STANDARD_NO_PAD.encode(&data);

    let prover_inputs = ProverInputs { r1cs, wtns };

    let prove_response = do_prove(prover_inputs).unwrap();
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

    let mut inputs: Vec<Fr> = Vec::new();
    for r in TEST_SUDOKU_CORRECT {
        let fr = bigint256_to_fr(&BigInteger256::new([r, 0, 0, 0]));
        inputs.push(fr);
    }

    let result = do_verify(&bad_proof_bytes, &pvk_bytes, &inputs);
    assert!(matches!(result, Err(GevulotError::CanonicalSerializeError)));

    let result = do_verify(&proof_bytes, &bad_pvk_bytes, &inputs);
    assert!(matches!(result, Err(GevulotError::MarlinVerifyError)));
}
