//! The consensus module defines interfaces and structures for the consensus mechanism,
//! including consensus rules and the consensus engine responsible for block validation,
//! forks management, difficulty adjustment, and chain selection.

/// [`StryiConsensusEngine`] lives here.
mod engine;

/// [`ChainIndex`] implementation.
mod index;

/// Static consensus rules definition.
mod rules;

/// Fork utilities.
mod forks;

/// Helper for the block disposition detection logic
mod classify;

/// Block validator implementation.
/// Performs block-level validation according to consensus rules,
/// current chain state (or overlay in case if validating fork's block), and UTXO set.
mod validator;

use crate::block::{Block, BlockHash};
use crate::error::StryiCoreError;
use futures::future::BoxFuture;
use std::collections::HashMap;
use std::error::Error;
use std::fmt::Debug;

// --- exports ---
pub use crate::storage::{BlockStorage, StorageStats, UndoStorage, UtxoStorage};
pub use engine::StryiConsensusEngine;
pub use rules::ConsensusConsts;
pub use validator::BlockValidator;

/// Reply message type for ConsensusEngine.
/// See [`ConsensusEngine::on_block`] method for more implementation details.
#[derive(Debug, Clone, PartialEq)]
pub enum ConsensusVerdict {
    /// Block was successfully applied to a canonical chain
    Applied {
        /// New complexity of the chain including this new block.
        new_chain_complexity: u64,
    },

    /// Block was successfully accepted but did not become part of the canonical chain
    Buffered,

    /// Block is already in the local chain.
    AlreadyIncludedInChain,

    /// Block is already buffered in a fork tree.
    AlreadyKnownInForkTree,

    /// Block was rejected for any reason like failed validation, etc.
    Rejected(StryiCoreError),

    /// Block caused reorganization in a local chain.
    /// It either was included by itself or with some fork it belongs to.
    CausedReorganization {
        /// map deleted block's hashes keyed to its pre-reorganization height.
        deleted_blocks: HashMap<u64, BlockHash>,
    },
}

/// The `ConsensusEngine` trait defines the interface for consensus mechanisms.
/// The API is ultra-high-level, caller does not perform any pre-validation or checks,
/// all that is the engine's responsibility.
/// This includes block validation, chain selection, difficulty adjustment, block saving, etc.
pub trait ConsensusEngine {
    type Error: Debug + Send + Error + Clone;

    /// Method called for each new block
    fn on_block(&mut self, block: Block) -> BoxFuture<'_, Result<ConsensusVerdict, Self::Error>>;
}

/// Common storage trait bundle used across consensus / forks / overlays.
///
/// Implementations may use **any** error type that satisfies the individual
/// storage trait bounds.  The consensus engine converts foreign errors into
/// `StryiCoreError` at each call site via `.map_err()`.
pub trait FullNodeStorage:
    UtxoStorage + BlockStorage + StorageStats + UndoStorage + Send + Sync + 'static
{
}

impl<T> FullNodeStorage for T where
    T: UtxoStorage + BlockStorage + StorageStats + UndoStorage + Send + Sync + 'static
{
}
