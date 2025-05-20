use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fmt::Debug;
use std::time::Instant;
use crate::block::{Block, BlockHash};
use crate::error::StryiCoreError;

/// Persistent map of blocks that are *off* the active chain.
pub trait ForkStorage: Send + Sync {
    type Err: Debug + Error + Send + 'static;

    /// Insert or overwrite an entry.
    fn put(&mut self, entry: ForkEntry) -> Result<(), Self::Err>;

    /// Get by hash.
    fn get(&self, hash: &BlockHash) -> Result<Option<ForkEntry>, Self::Err>;

    /// Remove a branch starting from `tip_hash` and walking back
    /// until the first hash that is *not* present in this storage.
    fn prune_branch(&mut self, tip_hash: &BlockHash) -> Result<(), Self::Err>;

    /// Return hashes of all current fork tips.
    fn tips(&self) -> Result<Vec<BlockHash>, Self::Err>;
}

/// Information about a forked block not in the active chain.
/// Stores the full block and cached fork metadata without duplicating header fields.
#[derive(Debug, Clone)]
pub struct ForkEntry {
    /// The block.
    pub block: Block,

    /// Cumulative difficulty of the fork up to and including this block.
    pub cumulative_difficulty: u128,

    /// The common known ancestor in the active chain where this fork diverged.
    pub common_ancestor: BlockHash,

    /// Timestamp when this entry was first seen (for TTL pruning).
    pub timestamp: Instant,
}

/// A tree of forked chains, indexed by block hash and by height.
/// This implementation relies on HashMaps to store forks and entries.
// Note: Should we consider using more advanced storage for forks?
#[derive(Debug, Default)]
pub struct InMemoryForkTree {
    /// Lookup for any fork entry by its block hash
    pub entries: HashMap<BlockHash, ForkEntry>,
    /// Index of forks by block height for quick access/pruning
    pub by_height: HashMap<u64, HashSet<BlockHash>>,
    //TODO: TTL for storing forks?
}


impl ForkStorage for InMemoryForkTree {
    type Err = StryiCoreError;

    fn put(&mut self, entry: ForkEntry) -> Result<(), Self::Err> {
        todo!()
    }

    fn get(&self, hash: &BlockHash) -> Result<Option<ForkEntry>, Self::Err> {
        todo!()
    }

    fn prune_branch(&mut self, tip_hash: &BlockHash) -> Result<(), Self::Err> {
        todo!()
    }

    fn tips(&self) -> Result<Vec<BlockHash>, Self::Err> {
        todo!()
    }
}