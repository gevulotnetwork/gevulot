use std::borrow::Cow;

use jsonrpsee::IntoResponse;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[allow(clippy::enum_variant_names)]
#[derive(Clone, Error, Debug, Serialize, Deserialize)]
pub enum RpcError {
    #[error("not found: {0}")]
    NotFound(String),

    #[error("invalid request: {0}")]
    InvalidRequest(String),

    #[error("unauthorized")]
    Unauthorized,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum RpcResponse<T: Clone> {
    Ok(T),
    Err(RpcError),
}

impl<T: Clone> RpcResponse<T> {
    pub fn unwrap(&self) -> T {
        if let RpcResponse::Ok(v) = self {
            v.clone()
        } else {
            // TODO: Consider better debug print here?
            panic!("unwrap");
        }
    }
}

impl<T: Clone + Serialize> IntoResponse for RpcResponse<T> {
    type Output = RpcResponse<T>;

    fn into_response(self) -> jsonrpsee::types::ResponsePayload<'static, Self::Output> {
        jsonrpsee::types::ResponsePayload::Result(Cow::Owned(self))
    }
}
