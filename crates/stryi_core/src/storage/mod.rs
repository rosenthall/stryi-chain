#[cfg(test)]
mod in_memory;
#[cfg(test)]
pub use in_memory::{InMemoryStorageError, StryiInMemoryStorage};

use crate::BlockUndo;
use crate::address::AccountAddress;
use crate::block::{Block, BlockHash};
use crate::transactions::{OutPoint, UTXO};
use futures::future::BoxFuture;
use std::collections::HashMap;
use std::error::Error;
use std::fmt::Debug;
use std::ops::RangeInclusive;

/// Trait representing a storage backend for UTXOs.
///
/// Implementers must provide `batch_put_utxos`, `batch_remove_utxos`, `batch_get_utxos` and `get_utxos_for_address`.
/// The single-item helpers `put_utxo`, `remove_utxo` and `get_utxo` have default implementations that simply
/// wrap their batch counterparts. Back-ends may override these helpers with specialized
/// single-item versions for performance or any other reason, but that is never required.
pub trait UtxoStorage: Send + Sync {
    type StorageError: Debug + Error + Send + Error;

    /// Insert **one or more** UTXOs in a single atomic operation.
    ///
    /// *Each entry is given as `(OutPoint, Utxo)`.*
    fn batch_put_utxos(
        &mut self,
        utxos: Vec<(OutPoint, UTXO)>,
    ) -> BoxFuture<'_, Result<(), Self::StorageError>>;

    /// Remove (mark as spent) **one or more** UTXOs in one call.
    ///
    /// If any outpoint is not present, implementation must return specific error.
    fn batch_remove_utxos(
        &mut self,
        outpoints: Vec<OutPoint>,
    ) -> BoxFuture<'_, Result<(), Self::StorageError>>;

    /// Batch-fetches a heterogeneous set of outpoints.
    /// Missing or already-spent entries must result in an error.
    fn batch_get_utxos<I>(
        &self,
        outpoints: I,
    ) -> BoxFuture<'_, Result<HashMap<OutPoint, UTXO>, Self::StorageError>>
    where
        I: IntoIterator<Item = OutPoint> + Send,
        I::IntoIter: Send;

    /// Gets all the UTXOs for the provided AccountAddress.
    /// note: Helpful for calculating account balance and constructing new transactions.
    fn get_utxos_for_address(
        &self,
        address: AccountAddress,
    ) -> BoxFuture<'_, Result<HashMap<OutPoint, UTXO>, Self::StorageError>>;

    // Default wrappers

    /// Insert or update **exactly one** UTXO.
    ///
    /// By default, this just forwards to [`batch_put_utxos`]. Override if you need
    fn put_utxo(
        &mut self,
        outpoint: OutPoint,
        utxo: UTXO,
    ) -> BoxFuture<'_, Result<(), Self::StorageError>> {
        Box::pin(async move { self.batch_put_utxos(vec![(outpoint, utxo)]).await })
    }

    /// Fetches **exactly one** UTXO. `None` means “not found or already spent”.
    /// By default, this just forwards to [`batch_get_utxos`]. Override if you need
    fn get_utxo(
        &self,
        outpoint: OutPoint,
    ) -> BoxFuture<'_, Result<Option<UTXO>, Self::StorageError>> {
        Box::pin(async move {
            self.batch_get_utxos(std::iter::once(outpoint))
                .await
                .map(|mut m| m.remove(&outpoint))
        })
    }

    /// Remove (mark spent) **exactly one** UTXO.
    ///
    /// By default, this just forwards to [`batch_remove_utxos`]. Override if you need
    fn remove_utxo(&mut self, outpoint: OutPoint) -> BoxFuture<'_, Result<(), Self::StorageError>> {
        Box::pin(async move { self.batch_remove_utxos(vec![outpoint]).await })
    }
}

/// Common type for incorrect ranges
#[derive(Debug)]
pub enum RangeError {
    InvalidRange { start: usize, end: usize },
}

/// Thread‑safe backend for persistent block storage.
/// NOTE: The single-item helpers `get_block_by_hash` and `get_block_by_height` have default implementations that simply wrap their batch counterparts
pub trait BlockStorage: Send + Sync {
    type StorageError: Debug + Error + Send + Error;

    /// Atomically inserts or overwrites a single block.
    fn put_block(&mut self, block: &Block) -> BoxFuture<'_, Result<(), Self::StorageError>>;

    /// Fetches **one or more** blocks by hash.
    ///
    /// * Each entry of the returned `HashMap` is guaranteed to exist;
    ///   all requested hashes **must** be present, otherwise the
    ///   implementation must return an error.
    ///  * Returned map is keyed by BlockHash
    fn batch_get_blocks_by_hashes(
        &self,
        hashes: Vec<BlockHash>,
    ) -> BoxFuture<'_, Result<HashMap<BlockHash, Block>, Self::StorageError>>;

    /// Fetches **one or more** blocks by height.
    ///
    /// Heights that lie beyond the current tip must trigger an error.
    /// * Each entry of the returned `HashMap` is guaranteed to exist;
    ///   all requested heights **must** be present, otherwise the
    ///   implementation must return an error.
    fn batch_get_blocks_by_heights<I>(
        &self,
        heights: I,
    ) -> BoxFuture<'_, Result<HashMap<u64, Block>, Self::StorageError>>
    where
        I: IntoIterator<Item = u64> + Send + Clone,
        I::IntoIter: Send;

    /// Returns all blocks whose heights lie in the **inclusive** range, keyed by their height.
    /// Must be `range.end > range.start`.
    /// Shall return an error if any of the blocks in this range is unavailable or if the range is incorrect.
    fn blocks_range(
        &self,
        range: RangeInclusive<usize>,
    ) -> BoxFuture<'_, Result<HashMap<u64, Block>, Self::StorageError>>;

    /// Returns `Ok(false)` if the block is absent.
    /// Propagates storage errors from the backend.
    fn block_exists(&self, hash: BlockHash) -> BoxFuture<'_, Result<bool, Self::StorageError>>;

    // -- default impls for singular operations--

    /// Retrieves a block by its hash.
    /// Returns `Ok(None)` if not found.
    /// By default, this just forwards to [`batch_get_blocks_by_hashes`]. Override if you need
    fn get_block_by_hash(
        &self,
        hash: BlockHash,
    ) -> BoxFuture<'_, Result<Option<Block>, Self::StorageError>> {
        Box::pin(async move {
            match self.batch_get_blocks_by_hashes(vec![hash]).await {
                Ok(mut map) => Ok(map.remove(&hash)),
                Err(e) => Err(e),
            }
        })
    }

    /// Retrieves a block by its height.
    /// Returns `Ok(None)` if not found.
    /// By default, this just forwards to [`batch_get_blocks_by_heights`]. Override if you need
    fn get_block_by_height(
        &self,
        height: u64,
    ) -> BoxFuture<'_, Result<Option<Block>, Self::StorageError>> {
        Box::pin(async move {
            match self.batch_get_blocks_by_heights([height]).await {
                Ok(map) => Ok(map.get(&height).map(|b| b.to_owned())),
                Err(e) => Err(e),
            }
        })
    }

    /// Validates an inclusive range.
    /// Returns an error converted from [`RangeError::InvalidRange`] if `start > end`.
    fn validate_range(range: &RangeInclusive<usize>) -> Result<(), Self::StorageError>
    where
        Self::StorageError: From<RangeError>,
    {
        let (start, end) = (*range.start(), *range.end());

        if start > end {
            Err(RangeError::InvalidRange { start, end }.into())
        } else {
            Ok(())
        }
    }
}

/// StorageStats defines high-level api to retrieve some statistics from current blockchain state.
pub trait StorageStats: Sync + Sync {
    type StorageError: Debug + Error + Send + Error;

    /// Tip height and its block hash.
    fn tip(&self) -> BoxFuture<'_, Result<(u64, BlockHash), Self::StorageError>>;

    /// Unix timestamp of the most recent successful write
    /// (`put_block` / `put_blocks`).
    fn last_updated(&self) -> BoxFuture<'_, Result<u64, Self::StorageError>>;

    /// Total number of blocks (equal to `tip.height + 1`).
    fn block_count(&self) -> BoxFuture<'_, Result<u64, Self::StorageError>>;

    /// Cumulative chain difficulty.
    fn chain_difficulty(&self) -> BoxFuture<'_, Result<u128, Self::StorageError>>;
}

/// Storage contract for persisting and retrieving `BlockUndo`.
///
/// *The consensus engine relies on these methods when it calls
/// `UtxoProcessor::rewind_block` during a reorganisation.*
pub trait UndoStorage {
    type StorageError: Error + Send + Sync + 'static;

    /// Saves undo data for the block identified by `hash`.
    /// Implementations **should** overwrite an existing record if present.
    fn put_block_undo(
        &self,
        hash: BlockHash,
        undo: BlockUndo,
    ) -> BoxFuture<'_, Result<(), Self::StorageError>>;

    /// Fetches undo data.
    /// Returns `Ok(None)` if the storage has no record for `hash`.
    fn get_block_undo(
        &self,
        hash: BlockHash,
    ) -> BoxFuture<'_, Result<Option<BlockUndo>, Self::StorageError>>;

    /// Deletes undo data once a block becomes *final* (optional).
    fn delete_block_undo(&self, hash: BlockHash) -> BoxFuture<'_, Result<(), Self::StorageError>>;
}
