//! The consensus module defines interfaces and structures for the consensus mechanism,
//! including consensus rules and the consensus engine responsible for block validation,
//! difficulty adjustment, and chain selection.

mod difficulty;
mod engine;
mod fork_overlay;
mod index;
mod rules;
mod validator;

use crate::block::{Block, BlockHash};
use crate::error::StryiCoreError;
use futures::future::BoxFuture;
use std::collections::HashMap;
use std::error::Error;
use std::fmt::Debug;

// --- exports ---
pub use engine::StryiConsensusEngine;
pub use rules::ConsensusConsts;
pub use validator::BlockValidator;

/// Reply message type for ConsensusEngine.
/// See ConsensusEngine::on_block method for more details.
#[derive(Debug, Clone, PartialEq)]
pub enum ConsensusOnBlockVerdict {
    /// Block belongs to some fork of the chain, but this fork's cumulative complexity is lower than local one.
    BufferedIntoForkTree {
        /// Common's ancestor block's hash and height
        common_ancestor_height: (BlockHash, u64),
    },

    /// Block was successfully applied to local chain
    Applied {
        /// New complexity of the chain including this new block.
        new_chain_complexity: u64,
    },

    /// Block is already in local chain.
    AlreadyIncludedInChain,

    /// Block is already buffered in fork tree.
    AlreadyKnownInForkTree,

    /// Block was rejected for any reason like failed validation, etc.
    Rejected(StryiCoreError),

    /// Block caused reorganization in local chain.
    /// It either was included by itself or with some fork it belongs to.
    CausedReorganization {
        /// HashMap with deleted block's hashes keyed by its pre-reorganization height.
        deleted_blocks: HashMap<u8, BlockHash>,
    },
}

/// The `ConsensusEngine` trait defines the interface for consensus mechanisms.
/// It provides the only method `on_block`
pub trait ConsensusEngine {
    type Error: Debug + Send + Error + Clone;

    /// Method called for each new block
    fn on_block(&mut self, block: Block)
    -> BoxFuture<Result<ConsensusOnBlockVerdict, Self::Error>>;
}
