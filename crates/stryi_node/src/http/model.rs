use serde::Serialize;
use utoipa::ToSchema;


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