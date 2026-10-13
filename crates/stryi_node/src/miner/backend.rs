use futures_util::future::BoxFuture;
use std::sync::Arc;
use stryi_core::StryiCoreError;
use stryi_core::block::BlockHash;
use stryi_core::consensus::ConsensusConsts;
use stryi_core::difficulty::{DifficultyCalc, HashThreshold, median_time_past};
use stryi_core::storage::StorageStats;
use stryi_storage::StryiStorage;
use tokio::sync::RwLock;
use tokio::sync::broadcast;

pub struct MiningContext {
    pub parent_height: u64,
    pub parent_hash: BlockHash,
    pub hash_threshold: HashThreshold,
    pub median_time_past: u64,
}

/// Single dependency the miner needs from the node layer.
/// Keeps the miner decoupled from concrete storage, consensus, and difficulty types.
pub trait MinerBackend: Send + Sync {
    /// Parent and branch history from one stable storage view.
    fn mining_context(&self) -> BoxFuture<'_, Result<MiningContext, StryiCoreError>>;

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
    fn mining_context(&self) -> BoxFuture<'_, Result<MiningContext, StryiCoreError>> {
        Box::pin(async move {
            let storage = self.storage.read().await;
            let (parent_height, parent_hash) = storage
                .tip()
                .await
                .map_err(|e| StryiCoreError::other(format!("cannot read chain tip: {e}")))?;

            let calc = DifficultyCalc::new(self.consensus_consts.pow)?;
            let hash_threshold = calc.next_hash_threshold(&*storage, parent_hash).await?;
            let median_time_past = median_time_past(&*storage, parent_hash).await?;

            Ok(MiningContext {
                parent_height,
                parent_hash,
                hash_threshold,
                median_time_past,
            })
        })
    }

    fn block_subsidy(&self, height: u64) -> u64 {
        self.consensus_consts.block_subsidy(height)
    }

    fn subscribe_tip_changes(&self) -> broadcast::Receiver<BlockHash> {
        self.tip_updates.resubscribe()
    }
}
