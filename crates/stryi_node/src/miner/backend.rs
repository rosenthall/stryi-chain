use std::sync::Arc;
use futures_util::future::BoxFuture;
use stryi_core::block::BlockHash;
use stryi_core::consensus::ConsensusConsts;
use stryi_core::storage::StorageStats;
use stryi_storage::{StryiStorage, StryiStorageError};
use tokio::sync::broadcast;
use tokio::sync::RwLock;

/// Single dependency the miner needs from the node layer.
/// Keeps the miner decoupled from concrete storage, consensus, and difficulty types.
pub trait MinerBackend: Send + Sync {
    /// Current chain tip: (height, block_hash).
    fn tip(&self) -> BoxFuture<'_, Result<(u64, BlockHash), StryiStorageError>>;

    /// Difficulty bits for a given block height.
    fn difficulty_bits(&self, height: u64) -> u8;

    /// Block subsidy (reward) for a given height.
    fn block_subsidy(&self, height: u64) -> u64;

    /// Subscribe to canonical tip changes.
    fn subscribe_tip_changes(&self) -> broadcast::Receiver<BlockHash>;
}

/// Concrete implementation of [`MinerBackend`] that bridges the miner
/// to the node's storage and consensus constants.
pub struct NodeMinerBackend {
    pub(crate) storage: Arc<RwLock<StryiStorage>>,
    pub(crate) consensus_consts: ConsensusConsts,
    pub(crate) tip_updates: broadcast::Receiver<BlockHash>,
}

impl MinerBackend for NodeMinerBackend {
    fn tip(
        &self,
    ) -> BoxFuture<'_, Result<(u64, BlockHash), StryiStorageError>> {
        Box::pin(async { self.storage.read().await.tip().await })
    }

    fn difficulty_bits(&self, height: u64) -> u8 {
        self.consensus_consts.difficulty_bits_for_height(height)
    }

    fn block_subsidy(&self, height: u64) -> u64 {
        self.consensus_consts.block_subsidy(height)
    }

    fn subscribe_tip_changes(&self) -> broadcast::Receiver<BlockHash> {
        self.tip_updates.resubscribe()
    }
}
