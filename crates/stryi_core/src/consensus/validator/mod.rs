use crate::block::Block;
use crate::consensus::ConsensusConsts;
use crate::difficulty::DifficultyCalc;
use crate::error::StryiCoreError;
use crate::storage::{BlockStorage, UtxoStorage};
use tracing::debug;

pub mod block;

mod header;

pub mod tx;

/// Validates a block against the supplied consensus rules.
///
/// The pipeline:
/// 1. Header checks (`header::validate_header`);
/// 2. Static block structure (`block::validate_block_structure`);
/// 3. Dynamic, UTXO-dependent (`block::validate_transactions`).
pub struct BlockValidator {
    pub consensus_consts: ConsensusConsts,
    pub difficulty_calc: DifficultyCalc,
}

impl BlockValidator {
    /// Creates a new block validator with the given consensus constants and difficulty calculator.
    pub fn new(consensus_consts: ConsensusConsts, difficulty_calc: DifficultyCalc) -> Self {
        Self {
            consensus_consts,
            difficulty_calc,
        }
    }

    /// Runs the full consensus pipeline.
    pub async fn validate<DB>(&self, block: &Block, db: &DB) -> Result<(), StryiCoreError>
    where
        DB: BlockStorage + UtxoStorage + Send + Sync,
    {
        debug!(
            "Validating block {} that consists of {} transactions",
            block.block_hash(),
            block.data.transactions.len()
        );

        let hash = block.block_hash();
        let short_hash = &hash.to_string()[..8];

        header::validate_header(block, &self.difficulty_calc, db).await?;
        debug!("Block {short_hash} passed header validation");

        block::validate_block_structure(block)?;
        debug!("Block {short_hash} passed structure validation");

        block::validate_transactions(block, &self.consensus_consts, db).await?;
        debug!("Block {short_hash} passed transaction validation");

        Ok(())
    }

    /// Validates a block for a fork: header history and transactions are both
    /// resolved against the fork-local view, which reads through to canonical
    /// storage unless the fork overrides the entry.
    pub async fn validate_for_fork<DB>(
        &self,
        block: &Block,
        view: &DB,
    ) -> Result<(), StryiCoreError>
    where
        DB: BlockStorage + UtxoStorage + Send + Sync,
    {
        debug!(
            "Validating fork block {} that consists of {} transactions",
            block.block_hash(),
            block.data.transactions.len()
        );

        let hash = block.block_hash();
        let short_hash = &hash.to_string()[..8];

        header::validate_header(block, &self.difficulty_calc, view).await?;
        debug!("Fork block {short_hash} passed header validation");

        block::validate_block_structure(block)?;
        debug!("Fork block {short_hash} passed structure validation");

        block::validate_transactions(block, &self.consensus_consts, view).await?;
        debug!("Fork block {short_hash} passed transaction validation");

        Ok(())
    }
}
