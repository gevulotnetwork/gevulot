use crate::types::rpc::RpcTransaction;
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

pub struct RpcClient {
    client: HttpClient,
}

impl RpcClient {
    pub fn new(url: impl AsRef<str>) -> Self {
        let client = HttpClientBuilder::default()
            .build(url)
            .expect("http client");
        RpcClient { client }
    }

    pub async fn send_transaction(&self, tx: &Transaction<Created>) -> Result<(), Box<dyn Error>> {
        let mut params = ArrayParams::new();
        params.insert(tx).expect("rpc params");

        let resp = self
            .client
            .request::<RpcResponse<()>, ArrayParams>("sendTransaction", params)
            .await
            .expect("rpc request");

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
            .expect("rpc request");

        resp.into()
    }

    pub async fn get_transaction(&self, tx_hash: &Hash) -> Result<RpcTransaction, Box<dyn Error>> {
        let mut params = ArrayParams::new();
        params.insert(tx_hash).expect("rpc params");

        let resp = self
            .client
            .request::<RpcResponse<RpcTransaction>, ArrayParams>("getTransaction", params)
            .await
            .expect("rpc request");

        resp.into()
    }
}
