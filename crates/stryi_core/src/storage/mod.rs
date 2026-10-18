#[cfg(test)]
mod in_memory;
#[cfg(test)]
pub use in_memory::StryiInMemoryStorage;

mod transition;
pub use transition::{apply_transition, validate_genesis};

use crate::BlockUndo;
use crate::address::AccountAddress;
use crate::block::{Block, BlockHash, BlockHeader};
use crate::consensus::ForkDbDelta;
use crate::difficulty::{ChainWork, HashThreshold};
use crate::error::StryiCoreError;
use crate::transactions::{OutPoint, Transaction, TransactionHash, UTXO};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::error::Error;
use std::ops::RangeInclusive;
use std::sync::Arc;
use thiserror::Error;

/// Common storage failures used by consensus and by persistent backends.
/// Backend errors keep their original source; missing data and invalid transitions
/// remain distinguishable from failures that stop further processing.
#[derive(Debug, Clone, Error)]
pub enum StorageError {
    #[error("UTXO {0:?} not found")]
    UtxoNotFound(OutPoint),

    #[error("Block {0} not found")]
    BlockNotFound(BlockHash),

    #[error("No block at height {0}")]
    HeightMissing(u64),

    #[error("Storage is not initialized")]
    Uninitialized,

    #[error("Invalid range: start={start}, end={end}")]
    InvalidRange { start: u64, end: u64 },

    #[error("Invalid chain transition: {0}")]
    InvalidTransition(String),

    #[error("Unsupported storage schema {0}")]
    UnsupportedSchema(u32),

    #[error("Storage corruption: {0}")]
    Corruption(String),

    #[error("Storage operation {operation} failed: {source}")]
    Backend {
        operation: String,
        #[source]
        source: Arc<dyn Error + Send + Sync>,
    },

    #[error("Storage commit failed: {0}")]
    Commit(#[source] Arc<dyn Error + Send + Sync>),

    #[error("Storage stopped after a previous failure")]
    Unhealthy,
}

impl StorageError {
    /// Whether storage can no longer be trusted for processing new blocks.
    /// A rejected transition or a missing requested entry does not poison storage.
    pub fn is_fatal(&self) -> bool {
        matches!(
            self,
            Self::Corruption(_) | Self::Backend { .. } | Self::Commit(_) | Self::Unhealthy
        )
    }
}

/// Ancestry and cumulative work of a block, retained even after detachment.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BlockIndexData {
    pub parent_hash: BlockHash,
    pub height: u64,
    pub chain_work: ChainWork,
}

/// Location of a transaction on the selected chain, removed when its block detaches.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TransactionIndexData {
    pub block_hash: BlockHash,
    pub block_height: u64,
    pub tx_index: u32,
}

/// A confirmed transaction resolved together with its checked block location.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfirmedTransaction {
    pub transaction: Transaction,
    pub location: TransactionIndexData,
}

/// Selected chain state committed together with its canonical height mappings.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChainState {
    /// Tip height and its block hash.
    pub latest_block: (u64, BlockHash),

    /// Timestamp of the tip block, rather than wall-clock commit time.
    pub last_update_time: u64,

    /// Total number of blocks (equal to `tip.height + 1`).
    pub blocks_count: u64,

    /// Cumulative work of the canonical chain.
    pub chain_work: ChainWork,
}

/// Chain identity and schema, initialized in the same transaction as genesis.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChainMetadata {
    /// Persistent format revision, interpreted and checked by the backend.
    pub schema_version: u32,
    pub chain_name: String,
    pub protocol_version: u64,
    pub created_unix_ts: i64,
}

/// Atomic change to the canonical chain state.
///
/// Validation and fork choice happen in consensus before calling storage;
/// here we just check continuity and update blocks, indexes, UTXOs, undo and stats in one tx.
pub enum ChainTransition<'a> {
    /// Save genesis block, initial allocations and chain metadata.
    /// Since every genesis hashes to zero, repeated init checks full block content
    /// and keeps the original creation timestamp.
    Genesis {
        block: &'a Block,
        metadata: &'a ChainMetadata,
    },

    /// Append one validated block extending the current canonical tip.
    Append { block: &'a Block },

    /// Apply prepared fork replacement from [`ForkDbOverlay`].
    /// Checks that canonical tip still matches `delta.expected_head` and writes
    /// net UTXO changes, undo and new blocks without re-running transactions.
    Reorganize { delta: ForkDbDelta },
}

/// New chain head and detached blocks returned after [`FullNodeStorage::apply_transition`].
#[derive(Debug, Clone, PartialEq)]
pub struct ChainTransitionResult {
    pub new_head: (u64, BlockHash),
    pub chain_work: ChainWork,

    /// Detached canonical blocks (tip-first). Block bodies stay in storage by hash.
    pub detached: Vec<(u64, BlockHash)>,
}

/// Trait representing a storage backend for UTXOs.
pub trait UtxoStorage {
    /// Fetches **exactly one** UTXO. `None` means "not found or already spent".
    fn get_utxo(&self, outpoint: OutPoint) -> Result<Option<UTXO>, StorageError>;

    /// Batch-fetches a heterogeneous set of outpoints.
    /// Missing or already-spent entries must result in an error.
    fn batch_get_utxos<I>(&self, outpoints: I) -> Result<HashMap<OutPoint, UTXO>, StorageError>
    where
        I: IntoIterator<Item = OutPoint>,
    {
        let mut result = HashMap::new();
        for outpoint in outpoints {
            let utxo = self
                .get_utxo(outpoint)?
                .ok_or(StorageError::UtxoNotFound(outpoint))?;

            result.insert(outpoint, utxo);
        }

        Ok(result)
    }

    /// Gets all the UTXOs for the provided AccountAddress.
    /// note: Helpful for calculating account balance and constructing new transactions.
    fn get_utxos_for_address(
        &self,
        address: AccountAddress,
    ) -> Result<HashMap<OutPoint, UTXO>, StorageError>;
}

/// Block lookup by height (canonical chain) and by hash (including detached blocks).
pub trait BlockStorage {
    /// Canonical block hash at this height, or `None` if the height is absent.
    fn get_block_hash_by_height(&self, height: u64) -> Result<Option<BlockHash>, StorageError>;

    /// Retrieves a block by its hash.
    /// Returns `Ok(None)` if not found.
    fn get_block_by_hash(&self, hash: BlockHash) -> Result<Option<Block>, StorageError>;

    /// Retrieves a block by its height.
    /// Returns `Ok(None)` if not found.
    fn get_block_by_height(&self, height: u64) -> Result<Option<Block>, StorageError>;

    fn get_block_header_by_hash(
        &self,
        hash: BlockHash,
    ) -> Result<Option<BlockHeader>, StorageError> {
        Ok(self.get_block_by_hash(hash)?.map(|block| block.header))
    }

    /// Ancestry and cumulative branch work (including detached blocks).
    fn get_block_index(&self, hash: &BlockHash) -> Result<Option<BlockIndexData>, StorageError>;

    /// Resolves a confirmed transaction along with its block location (`Ok(None)` if unconfirmed).
    fn get_transaction(
        &self,
        hash: &TransactionHash,
    ) -> Result<Option<ConfirmedTransaction>, StorageError>;

    /// Fetches **one or more** blocks by hash.
    ///
    /// * Each entry of the returned `HashMap` is guaranteed to exist;
    ///   all requested hashes **must** be present, otherwise the
    ///   implementation must return an error.
    ///  * Returned map is keyed by BlockHash
    fn batch_get_blocks_by_hashes(
        &self,
        hashes: Vec<BlockHash>,
    ) -> Result<HashMap<BlockHash, Block>, StorageError> {
        let mut result = HashMap::with_capacity(hashes.len());
        for hash in hashes {
            let block = self
                .get_block_by_hash(hash)?
                .ok_or(StorageError::BlockNotFound(hash))?;

            result.insert(hash, block);
        }

        Ok(result)
    }

    /// Fetches **one or more** blocks by height.
    ///
    /// Heights that lie beyond the current tip must trigger an error.
    /// * Each entry of the returned `HashMap` is guaranteed to exist;
    ///   all requested heights **must** be present, otherwise the
    ///   implementation must return an error.
    fn batch_get_blocks_by_heights<I>(
        &self,
        heights: I,
    ) -> Result<HashMap<u64, Block>, StorageError>
    where
        I: IntoIterator<Item = u64>,
    {
        let mut result = HashMap::new();
        for height in heights {
            let block = self
                .get_block_by_height(height)?
                .ok_or(StorageError::HeightMissing(height))?;

            result.insert(height, block);
        }

        Ok(result)
    }

    /// Returns all blocks whose heights lie in the **inclusive** range, keyed by their height.
    /// Shall return an error if any of the blocks in this range is unavailable or if the range is incorrect.
    /// A single-height range is valid; `start > end` is rejected.
    fn blocks_range(
        &self,
        range: RangeInclusive<u64>,
    ) -> Result<HashMap<u64, Block>, StorageError> {
        if range.start() > range.end() {
            return Err(StorageError::InvalidRange {
                start: *range.start(),
                end: *range.end(),
            });
        }

        self.batch_get_blocks_by_heights(range)
    }

    /// Returns `Ok(false)` if the block is absent.
    /// Propagates storage errors from the backend.
    fn block_exists(&self, hash: BlockHash) -> Result<bool, StorageError> {
        Ok(self.get_block_header_by_hash(hash)?.is_some())
    }
}

/// StorageStats defines high-level api to retrieve some statistics from current blockchain state.
pub trait StorageStats {
    /// Selected chain state. `None` means genesis has not been initialized.
    fn chain_state(&self) -> Result<Option<ChainState>, StorageError>;

    /// Chain metadata, present exactly when genesis and chain state are initialized.
    fn metadata(&self) -> Result<Option<ChainMetadata>, StorageError>;

    /// Tip height and its block hash.
    fn tip(&self) -> Result<(u64, BlockHash), StorageError> {
        Ok(self
            .chain_state()?
            .ok_or(StorageError::Uninitialized)?
            .latest_block)
    }

    /// Unix timestamp of the tip block, rather than wall-clock commit time.
    fn last_updated(&self) -> Result<u64, StorageError> {
        Ok(self
            .chain_state()?
            .ok_or(StorageError::Uninitialized)?
            .last_update_time)
    }

    /// Total number of blocks (equal to `tip.height + 1`).
    fn block_count(&self) -> Result<u64, StorageError> {
        Ok(self
            .chain_state()?
            .ok_or(StorageError::Uninitialized)?
            .blocks_count)
    }

    /// Cumulative work of the canonical chain.
    fn chain_work(&self) -> Result<ChainWork, StorageError> {
        Ok(self
            .chain_state()?
            .ok_or(StorageError::Uninitialized)?
            .chain_work)
    }
}

/// Storage contract for retrieving `BlockUndo`.
///
/// *The consensus engine relies on these methods when it calls
/// `UtxoProcessor::rewind_block` during a reorganisation.*
pub trait UndoStorage {
    /// Fetches undo data.
    /// Returns `Ok(None)` if the storage has no record for `hash`.
    fn get_block_undo(&self, hash: BlockHash) -> Result<Option<BlockUndo>, StorageError>;
}

/// Combined read trait for snapshots and transaction views.
/// Note: inside a write transaction this also sees uncommitted writes; use [`StorageReader::read`] for an immutable snapshot.
pub trait StorageRead: UtxoStorage + BlockStorage + StorageStats + UndoStorage {}

impl<T: UtxoStorage + BlockStorage + StorageStats + UndoStorage> StorageRead for T {}

/// Staged UTXO writes (inside a DB transaction or [`ForkDbOverlay`]).
/// Implementations must see their own uncommitted writes; on error the transaction/overlay is dropped.
pub trait UtxoWrite: UtxoStorage {
    /// Insert or update **one or more** UTXOs, including their address membership.
    /// Each entry is given as `(OutPoint, Utxo)`.
    fn batch_put_utxos(&mut self, utxos: Vec<(OutPoint, UTXO)>) -> Result<(), StorageError>;

    /// Remove (mark as spent) **one or more** UTXOs in one call.
    /// If any outpoint is not present, implementation must return specific error.
    fn batch_remove_utxos(&mut self, outpoints: Vec<OutPoint>) -> Result<(), StorageError>;
}

/// Write operations staged inside a storage transaction.
pub trait StorageWrite: StorageRead + UtxoWrite {
    /// Save block body, work index and canonical height/tx indexes.
    fn put_block(&mut self, block: &Block, index: &BlockIndexData) -> Result<(), StorageError>;

    /// Remove canonical height and tx indexes for a detached block (body, work index and undo stay by hash).
    fn remove_canonical_block(&mut self, block: &Block) -> Result<(), StorageError>;

    /// Saves undo data for the block identified by `hash`.
    /// Implementations **should** overwrite an existing record if present.
    fn put_block_undo(&mut self, hash: BlockHash, undo: BlockUndo) -> Result<(), StorageError>;

    /// Update stored chain state (tip, block count, cumulative work).
    fn put_chain_state(&mut self, state: &ChainState) -> Result<(), StorageError>;

    /// Save chain metadata during genesis init.
    fn put_metadata(&mut self, metadata: &ChainMetadata) -> Result<(), StorageError>;
}

/// Provides consistent read snapshots over storage.
pub trait StorageReader: Clone + Send + Sync + 'static {
    type Read<'a>: StorageRead + Send + Sync
    where
        Self: 'a;

    /// Returns a snapshot view for consistent multi-table reads (reuse a single view if you need
    /// multiple related reads without a reorg happening in between).
    fn read(&self) -> Result<Self::Read<'_>, StorageError>;
}

/// Storage backend capable of applying atomic chain transitions.
pub trait FullNodeStorage: StorageReader {
    /// Applies transition atomically in a single transaction.
    fn apply_transition(
        &self,
        transition: ChainTransition<'_>,
    ) -> Result<ChainTransitionResult, StorageError>;
}

/// Verifies that a block carrying the genesis marker really has the genesis
/// shape before its zero genesis work can be trusted.
///
/// `Block::block_hash()` and `Block::work()` treat every marker-carrying header
/// as the zero-hash genesis, so a header with the marker but a normal block
/// shape would otherwise slip past proof-of-work and work accounting. A marker
/// is only valid at height 0 with an empty previous hash, zero threshold,
/// zero timestamp and zero nonce.
pub fn verify_genesis_work_shape(block: &Block) -> Result<(), StryiCoreError> {
    verify_genesis_header_work_shape(&block.header)
}

pub fn verify_genesis_header_work_shape(header: &BlockHeader) -> Result<(), StryiCoreError> {
    if header.is_genesis()
        && (header.height != 0
            || header.previous_block_hash != BlockHash::empty()
            || header.hash_threshold != HashThreshold::ZERO
            || header.timestamp != 0
            || header.nonce != 0)
    {
        return Err(StryiCoreError::ConsensusValidationFailed {
            details: "Genesis marker is only valid on a height-0 block with an empty previous hash, zero threshold, timestamp and nonce".into(),
        });
    }

    Ok(())
}

/// Work contributed by a block to its branch, with the zero-work genesis
/// shortcut guarded by [`verify_genesis_work_shape`].
pub(crate) fn verified_block_work(block: &Block) -> Result<ChainWork, StryiCoreError> {
    verify_genesis_work_shape(block)?;
    Ok(block.work())
}
