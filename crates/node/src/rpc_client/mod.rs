use std::error::Error;

use jsonrpsee::{
    core::{client::ClientT, params::ArrayParams},
    http_client::{HttpClient, HttpClientBuilder},
};

use crate::types::{
    rpc::RpcResponse,
    transaction::{Created, TransactionTree, Validated},
    Hash, Transaction,
};

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

    pub async fn get_transaction(
        &self,
        tx_hash: &Hash,
    ) -> Result<Option<Transaction<Validated>>, Box<dyn Error>> {
        let mut params = ArrayParams::new();
        params.insert(tx_hash).expect("rpc params");

        let resp = self
            .client
            .request::<RpcResponse<Transaction<Validated>>, ArrayParams>("getTransaction", params)
            .await
            .expect("rpc request");

        match resp {
            RpcResponse::Ok(tx) => Ok(Some(tx)),
            _ => Ok(None),
        }
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

        Ok(resp.unwrap())
    }

    pub async fn get_tx(&self, tx_hash: &Hash) -> Result<Transaction<Validated>, Box<dyn Error>> {
        let mut params = ArrayParams::new();
        params.insert(tx_hash).expect("rpc params");
        println!("get_tx ja");

        let resp = self
            .client
            .request::<RpcResponse<Transaction<Validated>>, ArrayParams>("getTransaction", params)
            .await
            .expect("rpc request");

        Ok(resp.unwrap())
    }
}
