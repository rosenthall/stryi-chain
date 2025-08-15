use axum::Json;
use axum::response::{IntoResponse, Response};
use http::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::fmt;
use thiserror::Error;
use utoipa::ToSchema;
use stryi_core::address::AccountAddress;
use stryi_core::block::{Block, BlockHash};
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

    #[error("invalid identifier format for {resource_kind}. Message: {message}")]
    #[schema(title = "InvalidResourceIdError")]
    InvalidResourceId {
        /// The resource type (block, transaction, account, utxo)
        resource_kind: String,
        /// Raw identifier provided by the client
        requested: String,
        /// user-friendly explanation of the error and required format
        message: String,
    },

    #[error("Resource {resource} was not found")]
    #[schema(title = "ResourceNotFoundError")]
    ResourceNotFound {
        resource: ResourceKind,
    },

    // Keep string for logging at call sites; response body won’t leak it.
    #[schema(title = "UnexpectedError")]
    #[error("unexpected: {0}")]
    Unexpected(String),
}





/// Different values that can be used to query a block in the blockchain: hash or height.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(title = "BlockIdentifier", description = "Identifier for a block in the blockchain, either by height or hash.")]
pub enum BlockIdentifier {
    /// Block height (u64)
    #[schema(example = 42)]
    Height(u64),

    /// Block hash (in StryiChain's format, e.g. "Bx7e09ff05219c8e14e8ffe148a9b23a824748cfb77bf0d424f4aff4d2b5b30d73")
    #[schema(value_type = String, example = "Bx9b6d1c0f1a0e4a8e9f3d2c1b0a000000000000000000000000000000000")]
    Hash(BlockHash),
}


impl fmt::Display for BlockIdentifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BlockIdentifier::Height(h) => write!(f, "{h}"),
            BlockIdentifier::Hash(h) => write!(f, "{h}"),
        }
    }
}

impl TryFrom<&str> for BlockIdentifier {
    type Error = StryiNodeHttpApiError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        // Try height first (matches your current API semantics).
        if let Ok(height) = value.parse::<u64>() {
            return Ok(BlockIdentifier::Height(height));
        }

        // Try block hash next.
        match BlockHash::from_hash_string(value) {
            Ok(h) => Ok(BlockIdentifier::Hash(h)),
            Err(_) => Err(StryiNodeHttpApiError::InvalidResourceId {
                resource_kind: ResourceKind::Block(BlockIdentifier::Height(0)).as_str(),
                requested: value.to_owned(),
                message: "Expected block height (u64) or block hash in StryiChain's format (Bx...)".into(),
            }),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, ToSchema)]
pub enum ResourceKind {
    /// 'Block {height}' or 'Block {hash}'
    #[schema(value_type = String, examples("Block 42", "Block Bx7e09ff05219c8e14e8ffe148a9b23a824748cfb77bf0d424f4aff4d2b5b30d73"))]
    Block(BlockIdentifier),

    /// 'Transaction {hash}'
    #[schema(value_type = String, example = "Tx68e8dfa2225f7776c2f141e500bb27da09cb9e241dfc67b82573d7439ced28b2")]
    Transaction(TransactionHash),

    /// 'Account @xxxxxx...yyyyyy'
    #[schema(value_type = String, example = "@14d191976ed003e7568495792286f699ad695478")]
    Account(AccountAddress),

    /// 'Utxo {tx_hash}:{vout}'
    #[schema(value_type = String, example = "Tx68e8dfa2225f7776c2f141e500bb27da09cb9e241dfc67b82573d7439ced28b2:16")]
    Utxo(OutPoint),
}


impl ResourceKind {
    pub fn as_str(&self) -> String {
        match self {
            // 'Block {height}' or 'Block {hash}' for blocks
            ResourceKind::Block(identifier) => format!("Block {}", identifier),
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
            StryiNodeHttpApiError::InvalidResourceId { resource_kind, requested, message } => {

                let body = json!({
                    "error": "invalid_resource_id",
                    "message": message,
                    "details": {
                        "resource": {
                            "kind": resource_kind,
                        },
                        "requested": requested,
                    }
                });
                (StatusCode::BAD_REQUEST, body)
            }

            StryiNodeHttpApiError::ResourceNotFound{ resource} => {
                let body = json!({
                    "error": "resource_not_found",
                    "message": format!("{} was not found.", resource),
                    "details": {
                        "resource": {
                            "kind": resource.kind_tag(),
                            "label": resource.to_string(),
                        },
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
            resource: ResourceKind::Block(BlockIdentifier::Height(42)),
        };

        let (status, body) = err.into_status_and_json();
        assert_eq!(status, StatusCode::NOT_FOUND);

        let v: Value = body;
        assert_eq!(v["error"], "resource_not_found");
        assert_eq!(v["details"]["resource"]["kind"], "block");
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
