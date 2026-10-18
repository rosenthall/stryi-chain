use futures_util::future::BoxFuture;
use stryi_core::StryiCoreError;
use stryi_core::block::BlockHash;
use stryi_core::consensus::ConsensusConsts;
use stryi_core::difficulty::{DifficultyCalc, HashThreshold, median_time_past};
use stryi_core::storage::{StorageReader, StorageStats};
use stryi_storage::StryiStorage;
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
    pub(crate) storage: StryiStorage,
    pub(crate) consensus_consts: ConsensusConsts,
    pub(crate) tip_updates: broadcast::Receiver<BlockHash>,
}

impl MinerBackend for NodeMinerBackend {
    fn mining_context(&self) -> BoxFuture<'_, Result<MiningContext, StryiCoreError>> {
        let storage = self.storage.clone();
        let pow = self.consensus_consts.pow;

        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                let view = storage.read()?;
                let (parent_height, parent_hash) = view.tip()?;

                let calc = DifficultyCalc::new(pow)?;
                let hash_threshold = calc.next_hash_threshold(&view, parent_hash)?;
                let median_time_past = median_time_past(&view, parent_hash)?;

                Ok(MiningContext {
                    parent_height,
                    parent_hash,
                    hash_threshold,
                    median_time_past,
                })
            })
            .await
            .map_err(|e| StryiCoreError::other(format!("mining context worker failed: {e}")))?
        })
    }

    fn block_subsidy(&self, height: u64) -> u64 {
        self.consensus_consts.block_subsidy(height)
    }

    fn subscribe_tip_changes(&self) -> broadcast::Receiver<BlockHash> {
        self.tip_updates.resubscribe()
    }
}
