use stryi_core::block::Block;
use tokio::sync::mpsc;

/// All the channels and metadata that connect the miner to the node event loop.
/// Only present when mining is enabled for this node.
pub(crate) struct MinerBridge {
    /// Receiver for blocks mined locally.
    pub mined_blocks_receiver: mpsc::Receiver<Block>,
}
