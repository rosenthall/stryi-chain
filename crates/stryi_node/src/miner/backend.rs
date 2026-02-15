use futures_util::future::BoxFuture;
use stryi_core::StryiCoreError;
use stryi_core::block::{Block, BlockHash};
use stryi_core::consensus::ConsensusVerdict;
use stryi_storage::StryiStorageError;
use tokio::sync::broadcast;

/// Single dependency the miner needs from the node layer.
/// Keeps the miner decoupled from concrete storage, consensus, and difficulty types.
pub trait MinerBackend: Send + Sync {
    /// Current chain tip: (height, block_hash).
    fn tip(&self) -> BoxFuture<'_, Result<(u64, BlockHash), StryiStorageError>>;

    /// Difficulty bits for a given block height.
    fn difficulty_bits(&self, height: u64) -> u8;

    /// Block subsidy (reward) for a given height.
    fn block_subsidy(&self, height: u64) -> u64;

    /// Submit a mined block to the local consensus engine for validation.
    /// This is meant to be used before this block's propagation
    fn submit_block(&self, block: Block)
    -> BoxFuture<'_, Result<ConsensusVerdict, StryiCoreError>>;

    /// Subscribe to canonical tip changes.
    fn subscribe_tip_changes(&self) -> broadcast::Receiver<BlockHash>;
}
