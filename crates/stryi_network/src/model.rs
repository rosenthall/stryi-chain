use serde::{Deserialize, Serialize};
use stryi_core::address::AccountAddress;
use stryi_core::block::Block;

/// Represents a block that is optimized for network transmission.
/// Contains additional metadata useful for block propagation and consensus process
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BroadcastBlock {
    /// The actual block data
    pub block: Block,

    /// Address of the miner who produced this block
    pub miner_address: AccountAddress,

    /// Number of transactions in the block
    pub transactions_count: usize,

    /// Total fees that may be collected in this block
    pub total_fees: u64,

    /// Unix timestamp when this block was started to mine locally
    pub first_seen: u64,
}

impl BroadcastBlock {
    pub fn new(block: Block, miner_address: AccountAddress, first_seen: u64) -> Self {
        let transactions_count = block.data.transactions.len();
        let total_fees = 10000; // TODO: Actually calculate blocks fees somehow

        Self {
            block,
            miner_address,
            transactions_count,
            total_fees,
            first_seen,
        }
    }
}
