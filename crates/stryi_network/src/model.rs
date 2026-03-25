use crate::PeerId;
use serde::{Deserialize, Serialize};
use stryi_core::block::{Block, BlockHash};

/// Represents a block optimized for network transmission.
/// Contains additional metadata useful for block propagation, and the block itself
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BroadcastBlock {
    /// The actual block data
    pub block: Block,

    /// Peer id of the node that originally broadcast this block.
    /// Meant to only be used for logging and telemetry.
    pub origin_peer_id: PeerId,
}

impl BroadcastBlock {
    pub fn new(block: Block, origin_peer_id: PeerId) -> Self {
        debug_assert!(
            block.miner_address().is_some(),
            "BroadcastBlock requires a non-genesis block with a valid coinbase"
        );

        Self {
            block,
            origin_peer_id,
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
