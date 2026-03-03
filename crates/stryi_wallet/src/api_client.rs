//! Node HTTP API client.

use anyhow::{Context, Result};
use reqwest::Response;
use serde::{Deserialize, Serialize};
use stryi_core::transactions::TransactionHash;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UtxoEntry {
    pub txid: TransactionHash,
    pub vout: u32,
    pub value: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddressBalanceResponse {
    pub address: String,
    pub balance: u64,
    pub utxos: Vec<UtxoEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeStateResponse {
    pub chain_name: String,
    pub api_version: u32,
    pub height: u64,
    pub latest_block_hash: String,
    pub total_difficulty: u128,
    pub last_update_time: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BlockQueryResponse {
    pub hash: String,
    pub block: serde_json::Value,
}

#[derive(Debug, Serialize)]
pub struct SendTransactionRequest {
    pub raw_tx: String,
}

pub struct NodeClient {
    base_url: String,
    client: reqwest::Client,
}

impl NodeClient {
    /// Creates a new client targeting the given node base URL.
    pub fn new(base_url: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            client: reqwest::Client::new(),
        }
    }

    async fn ensure_success(response: Response, method: &str, url: &str) -> Result<Response> {
        let status = response.status();

        if status.is_success() {
            return Ok(response);
        }

        let body = response.text().await.unwrap_or_default();
        anyhow::bail!("{method} {url} returned {status}: {body}");
    }

    /// Fetches the balance and UTXO set for the given address.
    pub async fn get_balance(&self, address: &str) -> Result<AddressBalanceResponse> {
        let url = format!("{}/api/address/{}/balance", self.base_url, address);
        let response = self
            .client
            .get(&url)
            .send()
            .await
            .context("failed to connect to node")?;

        let response = Self::ensure_success(response, "GET", &url).await?;

        response
            .json()
            .await
            .context("failed to parse balance response")
    }

    /// Fetches the current chain state from the node.
    pub async fn get_nodestate(&self) -> Result<NodeStateResponse> {
        let url = format!("{}/api/nodestate", self.base_url);
        let response = self
            .client
            .get(&url)
            .send()
            .await
            .context("failed to connect to node")?;

        let response = Self::ensure_success(response, "GET", &url).await?;

        response
            .json()
            .await
            .context("failed to parse nodestate response")
    }

    /// Fetches a block by height or hash string.
    pub async fn get_block(&self, identifier: &str) -> Result<BlockQueryResponse> {
        let url = format!("{}/api/block/{}", self.base_url, identifier);
        let response = self
            .client
            .get(&url)
            .send()
            .await
            .context("failed to connect to node")?;

        let response = Self::ensure_success(response, "GET", &url).await?;

        response
            .json()
            .await
            .context("failed to parse block response")
    }

    /// Submits a base64-encoded signed transaction to the node.
    pub async fn send_transaction(&self, raw_tx_base64: &str) -> Result<String> {
        let url = format!("{}/api/tx", self.base_url);
        let body = SendTransactionRequest {
            raw_tx: raw_tx_base64.to_string(),
        };

        let response = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .context("failed to connect to node")?;

        let response = Self::ensure_success(response, "POST", &url).await?;

        response
            .text()
            .await
            .context("failed to read send_transaction response")
    }
}
