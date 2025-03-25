use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::info;

use stryi_core::{
    block::BlockHash,
    BlockUndo,
    consensus::{StryiConsensusEngine},
    storage::{BlockStorage, UtxoStorage},
};
use stryi_core::consensus::ConsensusEngine;
use crate::{StryiStorage, StryiStorageError};


/// Short-lived helper that performs chain reorganization in an atomic fashion.
/// This structure is intended for temporary use during a single reorganization process.
pub struct ChainReorganizer {
    /// Reference-counted pointer to the StryiStorage, protected by an async RwLock
    pub storage: Arc<RwLock<StryiStorage>>,
}

impl ChainReorganizer {
    /// Creates a new ChainReorganizer referencing the shared storage.
    pub fn new(storage: Arc<RwLock<StryiStorage>>) -> Self {
        Self { storage }
    }

    /// High-level orchestrator:
    ///  - Finds the common ancestor of `old_tip` and `new_tip`.
    ///  - Rolls back the old chain from `old_tip` down to (but not including) the ancestor.
    ///  - Applies and validates the new chain from the ancestor up to `new_tip`
    ///    by invoking the consensus engine’s `validate_and_apply_block` method.
    ///  - Updates the stats partition with the new best tip.
    ///
    /// The entire operation is executed under a single exclusive write lock to ensure atomicity.
    ///
    /// # Parameters
    /// - `old_tip`: Hash of the current best chain tip.
    /// - `new_tip`: Hash of the new best chain tip we want to switch to.
    /// - `consensus_engine`: The consensus engine with `StryiStorage` as its backing UTXO store
    ///    (`StryiConsensusEngine<StryiStorage>`). This is used for validating/applying blocks.
    pub async fn reorganize_with_validation(
        &self,
        old_tip: BlockHash,
        new_tip: BlockHash,
        consensus_engine: &StryiConsensusEngine<StryiStorage>,
    ) -> Result<(), StryiStorageError> {
        info!("Starting reorganization: old_tip={}, new_tip={}", old_tip, new_tip);

        // Acquire an exclusive write lock on storage.
        let mut store = self.storage.write().await;

        // 1) Find the common ancestor.
        let ancestor = self.find_common_ancestor(&mut *store, old_tip, new_tip).await?;
        info!("Common ancestor found: {}", ancestor);

        // 2) Roll back the old chain.
        self.rollback_chain(&mut *store, old_tip, ancestor).await?;
        info!("Rollback completed up to ancestor {}", ancestor);

        // 3) Apply (and validate) the new chain from ancestor to new_tip.
        self.apply_chain_with_validation(&mut *store, new_tip, ancestor, consensus_engine).await?;
        info!("Applied new chain up to new tip {}", new_tip);

        // 4) Update chain statistics to reflect new best tip.
        self.update_best_chain(&mut *store, new_tip).await?;
        info!("Updated chain state to new tip {}", new_tip);

        Ok(())
    }

    /// Finds the first common ancestor of `old_tip` and `new_tip` by climbing
    /// block indexes (heights) until they match.
    async fn find_common_ancestor(
        &self,
        store: &mut StryiStorage,
        mut hash1: BlockHash,
        mut hash2: BlockHash,
    ) -> Result<BlockHash, StryiStorageError> {

        // 1) Load index for each hash.
        let mut idx1 = store.get_block_index(&hash1)?;
        let mut idx2 = store.get_block_index(&hash2)?;

        // 2) Climb whichever branch is taller.
        while idx1.height > idx2.height {
            hash1 = idx1.parent_hash;
            idx1 = store.get_block_index(&hash1)?;
        }
        while idx2.height > idx1.height {
            hash2 = idx2.parent_hash;
            idx2 = store.get_block_index(&hash2)?;
        }

        // 3) Climb until both hashes match.
        while hash1 != hash2 {
            hash1 = idx1.parent_hash;
            idx1 = store.get_block_index(&hash1)?;

            hash2 = idx2.parent_hash;
            idx2 = store.get_block_index(&hash2)?;
        }

        Ok(hash1)
    }

    /// Rolls back blocks from `old_tip` down to (but not including) `ancestor_hash`,
    /// removing them from the storage’s block index and reapplying block undo data
    /// to restore the UTXOs.
    async fn rollback_chain(
        &self,
        store: &mut StryiStorage,
        mut tip_hash: BlockHash,
        ancestor_hash: BlockHash,
    ) -> Result<(), StryiStorageError> {
        info!("Starting rollback from tip {} down to ancestor {}", tip_hash, ancestor_hash);

        while tip_hash != ancestor_hash {
            // Load block index for current tip.
            let idx = store.get_block_index(&tip_hash)?;

            // Load `BlockUndo` data for the current block
            let undo = store.get_block_undo(&tip_hash).await?;
            info!("Rolling back block {} using undo data", tip_hash);

            // Unapply (roll back) that block’s UTXO changes
            self.unapply_block_undo(store, undo).await?;

            // Remove the block’s index entry now that it’s not on main chain
            store.remove_block_index(&tip_hash)?;
            info!("Removed block index for {}", tip_hash);

            // Move to the parent (going backward)
            tip_hash = idx.parent_hash;
        }

        Ok(())
    }

    /// Reverts the effects of a block using its undo data:
    /// - Restores spent UTXOs
    /// - Removes newly created UTXOs
    async fn unapply_block_undo(
        &self,
        store: &mut StryiStorage,
        undo: BlockUndo,
    ) -> Result<(), StryiStorageError> {
        // 1) Restore all spent UTXOs
        let mut to_put = Vec::new();
        for (op, utxo) in undo.spent_utxos {
            to_put.push((op, utxo));
        }
        store.batch_put_utxos(to_put).await?;

        // 2) Remove newly created outpoints
        let mut to_remove = Vec::new();
        for op in undo.created_outpoints {
            to_remove.push(op);
        }
        store.batch_remove_utxos(to_remove).await?;

        Ok(())
    }

    /// Gathers blocks from `new_tip` down to `ancestor_hash`, reverses them, and
    /// then applies each block in ascending order, validating them with
    /// `consensus_engine.validate_and_apply_block(...)`.
    async fn apply_chain_with_validation(
        &self,
        store: &mut StryiStorage,
        mut new_tip: BlockHash,
        ancestor_hash: BlockHash,
        consensus_engine: &StryiConsensusEngine<StryiStorage>,
    ) -> Result<(), StryiStorageError> {

        let mut path = Vec::new();
        // 1) Gather blocks from new_tip downward until ancestor is reached
        while new_tip != ancestor_hash {
            let block = store.get_block_by_hash(new_tip).await?;
            path.push(block);
            let idx = store.get_block_index(&new_tip)?;
            new_tip = idx.parent_hash;
        }

        // 2) Reverse so we apply them from ancestor -> new_tip
        path.reverse();

        // 3) Validate & apply each block in ascending order
        for block in path {
            info!("[reorganizer] Validating and applying block {}", block.block_hash());
            consensus_engine
                .validate_and_apply_block(&block, store)
                .await.expect("TODO: somehow handle validation error when reorganizing");
        }

        Ok(())
    }

    /// Updates the chain stats so that `latest_block` points to `new_tip_hash`.
    /// Also sets chain difficulty from the block index’s chain_work, and
    /// sets last_update_time from the block header’s timestamp.
    async fn update_best_chain(
        &self,
        store: &mut StryiStorage,
        new_tip_hash: BlockHash,
    ) -> Result<(), StryiStorageError> {
        // 1) read the new tip block
        let new_tip_block = store.get_block_by_hash(new_tip_hash).await?;

        // 2) retrieve current state, modify it
        let mut state = store.get_current_storage_state()?;

        // 3) set new best block
        state.latest_block = (new_tip_block.header.height as usize, new_tip_hash);

        // 4) set chain difficulty from block index
        let new_idx = store.get_block_index(&new_tip_hash)?;
        state.chain_difficulty = new_idx.chain_work as usize;

        // 5) set last update time from block header
        state.last_update_time = new_tip_block.header.timestamp as usize;

        // 6) store the updated state
        store.update_storage_state(state)?;

        Ok(())
    }
}
