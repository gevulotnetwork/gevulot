use serde::{Deserialize, Serialize};
use std::fmt::{Display, Formatter, Result};
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;

#[derive(Default, Clone, Deserialize, Serialize, Debug)]
pub struct ResponseHeader {
    pub success: bool,
    pub message: String,
}

#[derive(Default, Clone, Deserialize, Serialize, Debug)]
pub struct ProverInputs {
    pub r1cs: String,
    pub wtns: String,
}

#[derive(Default, Clone, Serialize, Deserialize, Debug)]
pub struct ProofInfo {
    pub algorithm: String,
    pub proof: String,
    pub vk: String,
}

#[derive(Default, Clone, Deserialize, Serialize, Debug)]
pub struct VerifyInputs {
    pub inputs: Vec<u64>,
    pub proof_info: ProofInfo,
}

#[derive(Default, Clone, Deserialize, Serialize, Debug)]
pub struct ProverResponse {
    pub header: ResponseHeader,
    pub proof_info: Option<ProofInfo>,
}

#[derive(Default, Clone, Deserialize, Serialize, Debug)]
pub struct VerifyResponse {
    pub header: ResponseHeader,
}

#[derive(Error, Debug)]
pub enum GevulotError {
    #[error("Error with base64 decoding")]
    Base64DecodeError,
    #[error("Error with base64 encoding")]
    Base64EncodeError,
    #[error("Error with canonical serialization")]
    CanonicalDeserializeError,
    #[error("Error with canonical deserialization")]
    CanonicalSerializeError,
    #[error("IO Error")]
    ErrorIo,
    #[error("Error with JSON serialization")]
    JsonDeserializeError,
    #[error("Error with JSON deserialization")]
    JsonSerializeError,
    #[error("Could not parse the r1cs file data")]
    R1csParseError,
    #[error("Could not parse the r1cs file data")]
    WtnsParseError,
    #[error("Error in Groth16 setup")]
    Groth16SetupError,
    #[error("Error in Groth16 verification")]
    Groth16VerifyError,
    #[error("Error in Marlin verification")]
    MarlinVerifyError,
    #[error("Filecoin window post error")]
    FilecoinWindowPostError,
    #[error("Filecoin winning post error")]
    FilecoinWinningPostError,
}

#[derive(PartialEq, Clone, Debug, Copy)]
pub enum GevulotAction {
    Deploy,
    Prove,
    Verify,
}
impl From<&str> for GevulotAction {
    fn from(input: &str) -> GevulotAction {
        match input {
            "deploy" => GevulotAction::Deploy,
            "prove" => GevulotAction::Prove,
            "verify" => GevulotAction::Verify,
            _ => panic!("invalid action string: {input}"),
        }
    }
}

#[derive(PartialEq, Clone, Debug, Copy)]
pub enum GevulotAlg {
    Marlin,
    Groth16,
    WinningPost,
    WindowPost,
}
impl From<&str> for GevulotAlg {
    fn from(input: &str) -> GevulotAlg {
        match input {
            "marlin" => GevulotAlg::Marlin,
            "groth16" => GevulotAlg::Groth16,
            "winning-post" => GevulotAlg::WinningPost,
            "window-post" => GevulotAlg::WindowPost,
            _ => panic!("invalid algorithm string: {input}"),
        }
    }
}

impl Display for GevulotAlg {
    fn fmt(&self, f: &mut Formatter) -> Result {
        match self {
            GevulotAlg::Marlin => write!(f, "marlin"),
            GevulotAlg::Groth16 => write!(f, "groth16"),
            GevulotAlg::WinningPost => write!(f, "winning-post"),
            GevulotAlg::WindowPost => write!(f, "window-post"),
        }
    }
}

pub const PROVER_SUCCESS: &str = "Prover succeeded";
pub const VERIFICATION_SUCCESS: &str = "Verification succeeded";
pub const VERIFICATION_FAIL: &str = "Verification failed";
pub const R1CS_PARSE_ERROR: &str = "error parsing R1CS file";
pub const WTNS_PARSE_ERROR: &str = "error parsing WTNS file";
pub const NOT_ON_CURVE: &str = "not on curve";

pub const TEST_SUDOKU_CORRECT: [u64; 81] = [
    6, 0, 3, 2, 0, 5, 4, 7, 0, 7, 9, 1, 4, 6, 0, 0, 2, 8, 5, 2, 4, 9, 0, 0, 1, 6, 0, 0, 4, 0, 0, 5,
    7, 3, 0, 0, 3, 1, 0, 6, 8, 0, 7, 4, 2, 0, 7, 0, 3, 4, 2, 0, 0, 0, 0, 0, 0, 5, 2, 4, 0, 0, 0, 1,
    0, 0, 7, 3, 0, 0, 0, 4, 0, 3, 0, 0, 9, 1, 0, 0, 7,
];

pub const TEST_SUDOKU_WRONG: [u64; 81] = [
    4, 0, 3, 2, 0, 5, 4, 7, 0, 7, 9, 1, 4, 6, 0, 0, 2, 8, 5, 2, 4, 9, 0, 0, 1, 6, 0, 0, 4, 0, 0, 5,
    7, 3, 0, 0, 3, 1, 0, 6, 8, 0, 7, 4, 2, 0, 7, 0, 3, 4, 2, 0, 0, 0, 0, 0, 0, 5, 2, 4, 0, 0, 0, 1,
    0, 0, 7, 3, 0, 0, 0, 4, 0, 3, 0, 0, 9, 1, 0, 0, 7,
];

pub fn get_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}
