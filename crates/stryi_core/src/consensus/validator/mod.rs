use crate::difficulty::DifficultyCalc;
use crate::storage::StorageStats;
use crate::{
    block::Block, consensus::ConsensusConsts, error::StryiCoreError, storage::UtxoStorage,
};
use tracing::debug;

/// Block-level validation helpers.
pub mod block;

/// Header-level consensus checks.
mod header;

/// Per transaction validation helpers
pub mod tx;

/// Validates a block against the supplied consensus rules.
///
/// The pipeline:
/// 1. Header checks (`header::validate_header`);
/// 2. Static block structure (`block::validate_block_structure`);
/// 3. Dynamic, UTXO-dependent (`block::validate_transactions`).
pub struct BlockValidator<ST>
where
    ST: StorageStats + Send + Sync + 'static,
{
    /// Immutable consensus parameters (subsidy, decay, etc.)
    pub consensus_consts: ConsensusConsts,

    /// Simple closure for convenient calculation of difficulty at a given height.
    pub difficulty_calc: DifficultyCalc<ST>,
}

impl<DB> BlockValidator<DB>
where
    DB: StorageStats + UtxoStorage + Send + Sync + 'static,
{
    /// Creates a new block validator with the given consensus constants and difficulty calculator.
    pub fn new(consensus_consts: ConsensusConsts, difficulty_calc: DifficultyCalc<DB>) -> Self {
        Self {
            consensus_consts,
            difficulty_calc,
        }
    }

    /// Runs the full consensus pipeline.
    pub async fn validate(&self, block: &Block, db: &DB) -> Result<(), StryiCoreError> {
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

    /// Validates a block for a fork: header is checked against canonical DB
    /// (difficulty is height-based), while transactions are validated against
    /// the fork-local UTXO state provided by `utxo_db`.
    pub async fn validate_for_fork<US>(
        &self,
        block: &Block,
        canonical_db: &DB,
        utxo_db: &US,
    ) -> Result<(), StryiCoreError>
    where
        US: UtxoStorage + Send + Sync,
    {
        debug!(
            "Validating fork block {} that consists of {} transactions",
            block.block_hash(),
            block.data.transactions.len()
        );

        let hash = block.block_hash();
        let short_hash = &hash.to_string()[..8];

        header::validate_header(block, &self.difficulty_calc, canonical_db).await?;
        debug!("Fork block {short_hash} passed header validation");

        block::validate_block_structure(block)?;
        debug!("Fork block {short_hash} passed structure validation");

        block::validate_transactions(block, &self.consensus_consts, utxo_db).await?;
        debug!("Fork block {short_hash} passed transaction validation");

        Ok(())
    }
}
