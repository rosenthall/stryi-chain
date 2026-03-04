use serde::{Deserialize, Serialize};
use stryi_core::address::AccountAddress;
use stryi_core::block::{Block, BlockHash};

/// Represents a block optimized for network transmission.
/// Contains additional metadata useful for block propagation, and the block itself
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BroadcastBlock {
    /// The actual block data
    pub block: Block,

    /// Address of the miner who produced this block
    pub miner_address: AccountAddress,

    /// Number of transactions in the block
    pub transactions_count: usize,

    /// Unix timestamp when this block was started to mine locally
    pub first_seen: u64,
}

impl BroadcastBlock {
    pub fn new(block: Block, miner_address: AccountAddress, first_seen: u64) -> Self {
        let transactions_count = block.data.transactions.len();

        Self {
            block,
            miner_address,
            transactions_count,
            first_seen,
        }
    }
}

/// Lightweight announcement of a node's current chain tip.
/// Broadcast via gossipsub on startup, periodically (heartbeat), and on chain reorganizations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChainTipAnnouncement {
    /// Height of the announcing node's chain tip.
    pub height: u64,
    /// Block hash of the tip
    pub tip_hash: BlockHash,
    /// Cumulative work, so nodes that see this announcement can compare it to their own immediately
    pub cumulative_work: u128,
}
