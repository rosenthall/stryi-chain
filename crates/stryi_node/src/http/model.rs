use serde::{Deserialize, Serialize};
use serde_json::Value;
use stryi_core::block::BlockHash;
use stryi_core::transactions::TransactionHash;
use utoipa::ToSchema;

/// Generic error body for the API, used in various endpoints.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ApiErrorBody {
    #[schema(example = "resource_not_found")]
    pub error: String,
    #[schema(example = "Block 42 was not found.")]
    pub message: String,
    /// Variant-specific details or null.
    #[schema(value_type = Object, nullable)]
    pub details: Option<Value>,
}

/// Payload returned by `/api/nodestate`.
#[derive(Serialize, ToSchema)]
pub struct NodeStateBody {
    /// Human-readable name of the network (e.g. “devnet”, “mainnet”).
    pub chain_name: String,

    /// Numeric version of this HTTP API.
    pub api_version: u32,

    /// Height of the current best block.
    pub height: u64,

    /// Hash of the current best block
    pub latest_block_hash: String,

    /// Cumulative chain difficulty up to the best block.
    pub total_difficulty: u128,

    /// Last time the storage layer was updated (Unix epoch seconds).
    pub last_update_time: u64,
    // TODO : Consider adding more fields in NoteStateBody e.g PeerId, grpc address+port, possibly the contacts of node's owner(?)
}

/// Request to send a transaction to the node, POST `/api/tx` endpoint
#[derive(Serialize, Deserialize, ToSchema)]
pub struct SendTransactionRequest {
    /// Raw transaction in base64-encoded format
    pub raw_tx: String,
}

/// Single unspent output in the balance response.
#[derive(Serialize, ToSchema)]
pub struct UtxoEntry {
    /// Transaction hash that created this output.
    #[schema(value_type = String, example = "Tx9a3b7c...")]
    pub txid: TransactionHash,

    /// Output index within the transaction.
    pub vout: u32,

    /// Value in the smallest currency unit.
    pub value: u64,
}

/// Payload returned by `GET /api/address/{addr}/balance`.
#[derive(Serialize, ToSchema)]
pub struct AddressBalanceResponse {
    /// The queried address.
    #[schema(example = "@dd0ca8155d946853106a5a5fb126ce17fdd9136c")]
    pub address: String,

    /// Total balance (sum of all UTXO values).
    pub balance: u64,

    /// List of unspent outputs owned by this address.
    pub utxos: Vec<UtxoEntry>,
}

/// Payload returned by `/api/blocks/{height}/hash` endpoint.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[schema(
    title = "BlockResponse",
    description = "Full block including header and transactions."
)]
pub struct BlockResponse {
    /// Hex block hash in the spec
    #[schema(value_type = String, example = "Bx7e09ff05219c8e14e8ffe148a9b23a824748cfb77bf0d424f4aff4d2b5b30d73")]
    pub hash: BlockHash,

    /// Full block
    #[schema(value_type = Object)]
    pub block: Value,
}
