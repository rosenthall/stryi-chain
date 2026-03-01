use anyhow::{Context, Result};
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

#[derive(Debug, Serialize)]
pub struct SendTransactionRequest {
    pub raw_tx: String,
}

pub struct NodeClient {
    base_url: String,
    client: reqwest::Client,
}

impl NodeClient {
    pub fn new(base_url: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            client: reqwest::Client::new(),
        }
    }

    pub async fn get_balance(&self, address: &str) -> Result<AddressBalanceResponse> {
        let url = format!("{}/api/address/{}/balance", self.base_url, address);
        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .context("failed to connect to node")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("GET {url} returned {status}: {body}");
        }

        resp.json()
            .await
            .context("failed to parse balance response")
    }

    pub async fn get_nodestate(&self) -> Result<NodeStateResponse> {
        let url = format!("{}/api/nodestate", self.base_url);
        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .context("failed to connect to node")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("GET {url} returned {status}: {body}");
        }

        resp.json()
            .await
            .context("failed to parse nodestate response")
    }

    pub async fn get_block(&self, identifier: &str) -> Result<serde_json::Value> {
        let url = format!("{}/api/block/{}", self.base_url, identifier);
        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .context("failed to connect to node")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("GET {url} returned {status}: {body}");
        }

        resp.json().await.context("failed to parse block response")
    }

    pub async fn send_transaction(&self, raw_tx_base64: &str) -> Result<String> {
        let url = format!("{}/api/tx", self.base_url);
        let body = SendTransactionRequest {
            raw_tx: raw_tx_base64.to_string(),
        };

        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .context("failed to connect to node")?;

        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();

        if !status.is_success() {
            anyhow::bail!("POST {url} returned {status}: {text}");
        }

        Ok(text)
    }
}
