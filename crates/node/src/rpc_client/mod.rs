use crate::types::rpc::{RpcError, RpcTransaction};
use crate::types::{
    rpc::RpcResponse,
    transaction::{Created, TransactionTree},
    Hash, Transaction,
};
use jsonrpsee::{
    core::{client::ClientT, params::ArrayParams},
    http_client::{HttpClient, HttpClientBuilder},
};
use std::error::Error;
use std::time::Duration;

/// A RPC client builder for connecting to the Gevulot network
#[derive(Debug)]
pub struct RpcClientBuilder {
    request_timeout: Duration,
}

impl Default for RpcClientBuilder {
    fn default() -> Self {
        Self {
            request_timeout: Duration::from_secs(60),
        }
    }
}

impl RpcClientBuilder {
    /// Set request timeout (default is 60 seconds).
    pub fn request_timeout(mut self, request_timeout: Duration) -> Self {
        self.request_timeout = request_timeout;
        self
    }

    /// Returns a [RpcClient] connected to the Gevulot network running at the URI provided.
    pub fn build(self, url: impl AsRef<str>) -> Result<RpcClient, Box<dyn Error>> {
        let client = HttpClientBuilder::default()
            .request_timeout(self.request_timeout)
            .build(url)?;
        Ok(RpcClient { client })
    }
}

pub struct RpcClient {
    client: HttpClient,
}

impl RpcClient {
    #[deprecated(note = "Please use `RpcClientBuilder` instead.")]
    pub fn new(url: impl AsRef<str>) -> Self {
        RpcClientBuilder::default().build(url).expect("http client")
    }

    pub async fn send_transaction(&self, tx: &Transaction<Created>) -> Result<(), Box<dyn Error>> {
        let mut params = ArrayParams::new();
        params.insert(tx).expect("rpc params");

        let resp = self
            .client
            .request::<RpcResponse<()>, ArrayParams>("sendTransaction", params)
            .await
            .or_else(|e| Err(Box::new(RpcError::RequestError(e.to_string()))))?;

        if let RpcResponse::Err(e) = resp {
            return Err(Box::new(e));
        }

        Ok(())
    }

    pub async fn get_tx_tree(&self, tx_hash: &Hash) -> Result<TransactionTree, Box<dyn Error>> {
        let mut params = ArrayParams::new();
        params.insert(tx_hash).expect("rpc params");

        let resp = self
            .client
            .request::<RpcResponse<TransactionTree>, ArrayParams>("getTransactionTree", params)
            .await
            .or_else(|e| Err(Box::new(RpcError::RequestError(e.to_string()))))?;

        resp.into()
    }

    pub async fn get_transaction(&self, tx_hash: &Hash) -> Result<RpcTransaction, Box<dyn Error>> {
        let mut params = ArrayParams::new();
        params.insert(tx_hash).expect("rpc params");

        let resp = self
            .client
            .request::<RpcResponse<RpcTransaction>, ArrayParams>("getTransaction", params)
            .await
            .or_else(|e| Err(Box::new(RpcError::RequestError(e.to_string()))))?;

        resp.into()
    }
}
