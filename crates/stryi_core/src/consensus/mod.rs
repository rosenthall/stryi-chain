mod engine;

mod index;

mod rules;

mod forks;

mod classify;

/// Block validator implementation.
/// Performs block-level validation according to consensus rules,
/// current chain state (or overlay in case if validating fork's block), and UTXO set.
mod validator;

use crate::block::{Block, BlockHash};
use crate::difficulty::ChainWork;
use crate::error::StryiCoreError;
use std::error::Error;
use std::fmt::Debug;

// --- exports ---
pub use crate::storage::{
    BlockStorage, FullNodeStorage, StorageReader, StorageStats, UndoStorage, UtxoStorage,
};
pub use engine::StryiConsensusEngine;
pub use forks::overlay::{ForkDbDelta, ForkDbOverlay};
pub use rules::ConsensusConsts;
pub use validator::BlockValidator;
#[cfg(test)]
pub(crate) use validator::block::validate_block_structure;

/// Committed consensus change returned after storage transition succeeds.
#[derive(Debug, Clone)]
pub struct CommittedTransition {
    pub new_head: (u64, BlockHash),
    pub chain_work: ChainWork,
    /// Newly attached canonical blocks (parent-first).
    pub attached: Vec<Block>,
    /// Detached canonical blocks (tip-first).
    pub detached: Vec<(u64, BlockHash)>,
}

/// Reply message type for ConsensusEngine.
/// See [`ConsensusEngine::on_block`] method for more implementation details.
#[derive(Debug, Clone)]
pub enum ConsensusVerdict {
    /// Block was successfully applied to a canonical chain
    Applied { transition: CommittedTransition },

    /// Block was successfully accepted but did not become part of the canonical chain
    Buffered,

    /// Block is already in the local chain.
    AlreadyIncludedInChain,

    /// Block is already buffered in a fork tree.
    AlreadyKnownInForkTree,

    /// Block was rejected during validation or chain processing.
    Rejected(StryiCoreError),

    /// Block caused reorganization in a local chain.
    /// The reorganization may be triggered by this block alone
    /// or by the fork branch that includes it.
    CausedReorganization { transition: CommittedTransition },
}

/// The `ConsensusEngine` trait defines the interface for consensus mechanisms.
/// The API is ultra-high-level, caller does not perform any pre-validation or checks,
/// all that is the engine's responsibility.
/// This includes block validation, chain selection, difficulty adjustment, block saving, etc.
pub trait ConsensusEngine {
    type Error: Debug + Send + Error + Clone;

    /// Method called for each new block.
    fn on_block(&mut self, block: Block) -> Result<ConsensusVerdict, Self::Error>;
}
