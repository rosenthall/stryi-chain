use crate::block::{Block, BlockHash};
use dashmap::{DashMap, mapref::one::RefMut};
use std::time::Instant;

/// Metadata for a competing fork tip.
/// Represents the head of an alternative branch that diverges from the
/// canonical chain at `common_ancestor`.
/// Used for fork-choice comparison and reorganization handling.
#[derive(Clone, Debug)]
pub struct ForkEntry {
    /// Tip block of the fork branch
    pub tip: BlockHash,

    /// Cumulative work at the tip
    pub cumulative_work: u128,

    /// Lowest common ancestor with the canonical chain
    pub common_ancestor: BlockHash,

    /// Time when fork was observed
    pub timestamp: Instant,

    /// Full block data
    pub block: Block,
}

pub trait ForksRead {
    /// existence check
    fn has(&self, hash: &BlockHash) -> bool;

    /// immutable lookup
    fn get(&self, hash: &BlockHash) -> Option<ForkEntry>;

    /// Best fork by cumulative work
    fn best_fork(&self) -> Option<ForkEntry>;

    /// mutable guarded access
    fn get_mut(&self, hash: &BlockHash) -> Option<RefMut<BlockHash, ForkEntry>>;

    /// Iterate over all forks (read-only snapshot)
    fn all(&self) -> Vec<ForkEntry>;
}

pub trait ForksWrite: ForksRead {
    /// insert or replace fork entry
    fn insert(&self, entry: ForkEntry);

    /// remove fork by tip hash
    fn remove(&self, tip: &BlockHash);

    /// optional maintenance hook
    fn maintenance(&self);
}

#[derive(Default)]
// Storage of all the competing forks.
pub struct ForkRegistry {
    inner: DashMap<BlockHash, ForkEntry>,
}

impl ForkRegistry {
    /// Create an empty registry
    pub fn new() -> Self {
        Self {
            inner: DashMap::new(),
        }
    }

    /// Number of tracked forks
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
}

impl ForksRead for ForkRegistry {
    fn has(&self, hash: &BlockHash) -> bool {
        self.inner.contains_key(hash)
    }

    fn get(&self, hash: &BlockHash) -> Option<ForkEntry> {
        self.inner.get(hash).map(|e| e.value().clone())
    }

    fn best_fork(&self) -> Option<ForkEntry> {
        self.inner
            .iter()
            .max_by_key(|e| e.value().cumulative_work)
            .map(|e| e.value().clone())
    }

    fn get_mut(&self, hash: &BlockHash) -> Option<RefMut<BlockHash, ForkEntry>> {
        self.inner.get_mut(hash)
    }

    fn all(&self) -> Vec<ForkEntry> {
        self.inner.iter().map(|e| e.value().clone()).collect()
    }
}

impl ForksWrite for ForkRegistry {
    fn insert(&self, entry: ForkEntry) {
        self.inner.insert(entry.tip, entry);
    }

    fn remove(&self, tip: &BlockHash) {
        self.inner.remove(tip);
    }

    /// IMPLEMENTATION DETAIL: Not implemented
    fn maintenance(&self) {}
}
