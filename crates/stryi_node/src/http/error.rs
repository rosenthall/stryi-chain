use axum::Json;
use axum::response::{IntoResponse, Response};
use http::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::fmt;
use thiserror::Error;
use utoipa::ToSchema;
use stryi_core::address::AccountAddress;
use stryi_core::transactions::{OutPoint, TransactionHash};

/// Minimal, structured reasons for a bad transaction.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, ToSchema, Error)]
#[serde(rename_all = "snake_case")]
pub enum BadTxReason {
    #[error("base64_decode")]
    Base64Decode,
    #[error("bincode_deserialize")]
    BincodeDeserialize,
    #[error("recover_public_key")]
    RecoverPublicKey,
    #[error("invalid_signature")]
    InvalidSignature,
}

/// Error type for StryiNode HTTP API
#[derive(Debug, Serialize, Deserialize, Eq, PartialEq, Clone, ToSchema, Error)]
#[serde(rename_all = "snake_case")]
pub enum StryiNodeHttpApiError {
    #[error("invalid transaction: {reason}")]
    #[schema(title = "BadTransactionError")]
    BadTransaction {
        reason: BadTxReason,
        #[serde(skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },

    #[error("{resource} not found: {requested}")]
    #[schema(title = "ResourceNotFoundError")]
    ResourceNotFound {
        resource: ResourceKind,
        requested: String,
    },

    // Keep string for logging at call sites; response body won’t leak it.
    #[schema(title = "UnexpectedError")]
    #[error("unexpected: {0}")]
    Unexpected(String),
}




/// Doc-only schema for OutPoint (txid + vout) shown in OpenAPI.
#[derive(Serialize, Deserialize, ToSchema)]
pub struct UtxoRefDoc {
    /// Hex transaction id
    #[schema(example = "9b6d1c0f1a0e4a8e9f3d2c1b0a...")]
    pub txid: String,
    /// Output index
    #[schema(example = 0)]
    pub vout: u32,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, ToSchema)]
pub enum ResourceKind {
    /// 'Block {height}'
    Block(u64),

    /// 'Transaction {hash}' – document as string
    #[schema(value_type = String, example = "9b6d1c0f1a0e4a8e9f3d2c1b0a...")]
    Transaction(TransactionHash),

    /// 'Account @xxxxxx...yyyyyy' – document as string
    #[schema(value_type = String, example = "@dd0ca8...d9136c")]
    Account(AccountAddress),

    /// 'Utxo {tx_hash}:{vout}' – document as object while keeping OutPoint at runtime
    #[schema(value_type = String, example = "todo:todo")]
    Utxo(OutPoint),
}

impl ResourceKind {
    pub fn as_str(&self) -> String {
        match self {
            // 'Block {height}' for blocks
            ResourceKind::Block(height) => format!("Block {}", height),
            // 'Account {partial_hash}' for accounts
            // where partial_hash is the first 7 characters and last 6 characters of the account
            // hash, e.g. 'Account @123456...789abc'
            ResourceKind::Account(hash) => {
                let hash_str = hash.to_string();
                let mut chars = hash_str.chars();
                let prefix: String = chars.by_ref().take(7).collect();
                let suffix: String = hash_str.chars().rev().take(6).collect::<String>().chars().rev().collect();
                format!("Account {}...{}", prefix, suffix)
            }
            // 'Transaction {hash}' for transactions
            ResourceKind::Transaction(hash) => format!("Transaction {}", hash),
            // 'Utxo {tx_hash}:{vout}' for UTXOs
            ResourceKind::Utxo(outpoint) => format!("Utxo {}:{}", outpoint.txid, outpoint.vout),
        }
    }


    /// Machine-oriented kind label (stable, lowercase snake_case).
    fn kind_tag(&self) -> &'static str {
        match self {
            ResourceKind::Block(_) => "block",
            ResourceKind::Transaction(_) => "transaction",
            ResourceKind::Account(_) => "account",
            ResourceKind::Utxo(_) => "utxo",
        }
    }
}


impl fmt::Display for ResourceKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.as_str())
    }
}

impl StryiNodeHttpApiError {
    // Small helper so tests can assert the JSON body without dealing with HTTP body types.
    fn into_status_and_json(self) -> (StatusCode, serde_json::Value) {
        match self {
            StryiNodeHttpApiError::BadTransaction { reason, message } => {
                let body = json!({
                    "error": "bad_transaction",
                    "message": message.unwrap_or_else(|| "The transaction is invalid.".to_string()),
                    "details": { "reason": reason },
                });
                (StatusCode::BAD_REQUEST, body)
            }
            StryiNodeHttpApiError::ResourceNotFound{ resource, requested} => {
                let body = json!({
                    "error": "resource_not_found",
                    "message": format!("{} was not found.", resource),
                    "details": {
                        "resource": {
                            "kind": resource.kind_tag(),
                            "label": resource.to_string(),
                        },
                        "requested": requested,
                    }
                });
                (StatusCode::NOT_FOUND, body)
            }
            StryiNodeHttpApiError::Unexpected(_internal) => {
                let body = json!({
                    "error": "unexpected_error",
                    "message": "An unexpected error occurred.",
                });
                (StatusCode::INTERNAL_SERVER_ERROR, body)
            }
        }
    }
}

/// `IntoResponse` so handlers can return `Result<_, StryiNodeHttpApiError>`.
impl IntoResponse for StryiNodeHttpApiError {
    fn into_response(self) -> Response {
        let (status, value) = self.into_status_and_json();
        (status, Json(value)).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::StatusCode;
    use serde_json::Value;

    #[test]
    fn display_resource_kind_account() {
        let address = AccountAddress::from_hash_string("@dd0ca8155d946853106a5a5fb126ce17fdd9136c")
            .expect("valid address");
        let rk = ResourceKind::Account(address);
        assert_eq!(rk.to_string(), "Account @dd0ca8...d9136c");
    }

    #[test]
    fn bad_tx_status_and_body() {
        let err = StryiNodeHttpApiError::BadTransaction {
            reason: BadTxReason::InvalidSignature,
            message: None,
        };
        let (status, body) = err.into_status_and_json();
        assert_eq!(status, StatusCode::BAD_REQUEST);

        let v: Value = body;
        assert_eq!(v["error"], "bad_transaction");
        assert_eq!(v["details"]["reason"], "invalid_signature");
        assert!(v["message"].is_string());
    }

    #[test]
    fn not_found_status_and_body() {
        let err = StryiNodeHttpApiError::ResourceNotFound {
            resource: ResourceKind::Block(42),
            requested: "height=42".into()
        };

        let (status, body) = err.into_status_and_json();
        assert_eq!(status, StatusCode::NOT_FOUND);

        let v: Value = body;
        assert_eq!(v["error"], "resource_not_found");
        assert_eq!(v["details"]["resource"]["kind"], "block");
        assert_eq!(v["details"]["requested"], "height=42");
        assert!(v["message"].as_str().unwrap().contains("Block 42"));
    }

    #[test]
    fn unexpected_status_and_body() {
        let err = StryiNodeHttpApiError::Unexpected("boom".into());
        let (status, body) = err.into_status_and_json();
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);

        let v: Value = body;
        assert_eq!(v["error"], "unexpected_error");
        assert!(v["message"].is_string());
        // internal string is intentionally not exposed
        assert!(v.get("details").is_none());
    }
}
