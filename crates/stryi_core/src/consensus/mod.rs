//! The consensus module defines interfaces and structures for the consensus mechanism,
//! including consensus rules and the consensus engine responsible for block validation,
//! difficulty adjustment, and chain selection.

mod rules;
mod engine;

use std::error::Error;
use std::fmt::Debug;
use crate::block::Block;
use crate::storage::UtxoStorage;

pub use rules::ConsensusRules;
pub use engine::StryiConsensusEngine;

/// The `ConsensusEngine` trait defines the interface for consensus mechanisms.
/// It provides methods for validating blocks, adjusting difficulty, and selecting the best chain among forks.
pub trait ConsensusEngine {
    type Error: Debug + Send + Error + Clone;
    type UtxoDatabase: UtxoStorage + Send + Sync;

    /// Validates a given block according to consensus rules.
    ///
    /// # Parameters
    /// - `block`: Reference to the block to be validated.
    ///
    /// # Returns
    /// - `Ok(())` if the block is valid according to consensus rules.
    /// - `Err(Self::Error)` if the block fails validation.
    async fn validate_block(&self, block: &Block, utxo_storage: &mut Self::UtxoDatabase) -> Result<(), Self::Error>;

    /// Adjusts the difficulty based on the current chain state.
    ///
    /// # Parameters
    /// - `chain`: A slice of blocks representing the current chain state.
    ///
    /// # Returns
    /// - `Ok(new_difficulty)` with the adjusted difficulty if successful.
    /// - `Err(Self::Error)` if the adjustment process fails.
    async fn adjust_difficulty(&mut self, chain: &[Block]) -> Result<u8, Self::Error>;

    /// Chooses the best chain among multiple forks based on cumulative difficulty or other criteria.
    ///
    /// # Parameters
    /// - `chains`: A vector of possible chains, where each chain is represented as a vector of blocks.
    ///
    /// # Returns
    /// - `Ok(best_chain)` containing the selected best chain.
    /// - `Err(Self::Error)` if chain selection fails.
    async fn select_chain(&self, chains: Vec<Vec<Block>>) -> Result<Vec<Block>, Self::Error>;
}
