use crate::block::BlockHash;
use crate::consensus::forks::overlay::ForkDbOverlay;
use crate::storage::{BlockStorage, StorageStats, UndoStorage, UtxoStorage};
use dashmap::DashMap;

/// Implementation of fork overlay.
/// Forks may have own overlay over the main chain state,
/// and this module implements that logic.
pub mod overlay;

/// A tree‐based structure for managing blockchain forks:
/// keeps orphaned blocks indexed by hash and height,
/// tracks each fork’s cumulative difficulty and divergence point,
/// and provides efficient ancestor discovery, chain reconstruction
pub mod forktree;

/// Manager for overlays associated with different forks.
pub struct OverlaysManager<DB>
where
    DB: UtxoStorage + BlockStorage + StorageStats + UndoStorage + Send + Sync + 'static,
{
    /// Inner mapping of LCA block to its corresponding fork database overlay.
    inner: DashMap<ForkOverlayId, ForkDbOverlay<DB>>,
}

/// Unique identifier for a fork overlay.
#[derive(Clone, PartialEq, Hash, Eq)]
pub struct ForkOverlayId {
    /// The block hash of the fork's lowest common ancestor (LCA) with the main chain.
    pub lca_block_hash: BlockHash,
    // TODO: update fork overlay id.
}

impl<DB> OverlaysManager<DB>
where
    DB: UtxoStorage + BlockStorage + StorageStats + UndoStorage + Send + Sync + 'static,
{
    /// Creates a new OverlaysManager instance.
    pub fn new() -> Self {
        Self {
            inner: DashMap::new(),
        }
    }

    /// Returns the number of overlays currently managed.
    pub fn overlays_amount(&self) -> usize {
        0
    }
}
