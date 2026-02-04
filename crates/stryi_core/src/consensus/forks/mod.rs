use crate::StryiCoreError;
use crate::block::{Block, BlockHash};
use crate::consensus::FullNodeStorage;
use crate::consensus::forks::overlay::ForkDbOverlay;
use crate::storage::{BlockStorage, UndoStorage};
use crate::transactions::UtxoProcessor;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Implementation of the overlay mechanism for forks.
pub mod overlay;

/// Implementation of the fork registry
pub mod registry;

/// Represents the state of a blockchain fork.
/// This structure contains metadata and fork-local state.
pub struct ForkState<DB>
where
    DB: FullNodeStorage,
{
    /// Lowest common ancestor with the canonical chain
    pub fork_root: BlockHash,

    /// Fork-local state overlay
    pub overlay: ForkDbOverlay<DB>,

    pub tip: BlockHash,
    pub cumulative_work: u128,
}

impl<DB> ForkState<DB>
where
    DB: FullNodeStorage,
{
    /// Create a new fork starting from a canonical block
    pub fn new(base: Arc<RwLock<DB>>, fork_root: BlockHash, base_work: u128) -> Self {
        Self {
            fork_root,
            tip: fork_root,
            overlay: ForkDbOverlay::new(base, base_work),
            cumulative_work: base_work,
        }
    }

    /// Apply a block to the fork state
    pub async fn apply_block(
        &mut self,
        block: &Block,
        utxo: &UtxoProcessor,
    ) -> Result<(), StryiCoreError> {
        // Apply transactions to fork-local UTXO state
        let undo = utxo.apply_block(block, &mut self.overlay).await?;

        // Store block and undo in the overlay
        self.overlay.put_block(block).await?;
        self.overlay
            .put_block_undo(block.block_hash(), undo)
            .await?;

        // Update fork tip and work
        self.tip = block.block_hash();
        self.cumulative_work += 1u128 << block.header.difficulty_bits;

        Ok(())
    }
}
