use serde::{Deserialize, Serialize};

use super::Program;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Deployment {
    pub prover: Program,
    pub verifier: Program,
    pub signature: String,
}
