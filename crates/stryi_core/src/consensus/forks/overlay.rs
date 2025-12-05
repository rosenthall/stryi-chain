use crate::BlockUndo;
use crate::address::AccountAddress;
use crate::block::{Block, BlockHash};
use crate::error::{StorageLayer, StryiCoreError};
use crate::storage::{BlockStorage, StorageStats, UndoStorage, UtxoStorage};
use crate::transactions::{OutPoint, UTXO};
use dashmap::{DashMap, DashSet};
use futures::future::BoxFuture;
use std::collections::HashMap;
use std::future::ready;
use std::range::RangeInclusive;
use std::sync::Arc;

/// An overlay database for a specific fork.
/// Stores only fork-specific delta over actual db in memory, delegating reads
/// to the base database when no fork-local entry exists.
pub struct ForkDbOverlay<DB>
where
    DB: UtxoStorage + BlockStorage + StorageStats + UndoStorage + Send + Sync + 'static,
{
    /// canonical state.
    /// by default, all reads are delegated to this DB unless overridden in the overlay
    base: Arc<DB>,

    /// cumulative work up to (and incl.) the fork point
    inherited_work: u128,

    /// new or overridden UTXOs on this fork
    utxo_delta: DashMap<OutPoint, UTXO>,

    /// outpoints that spent from base by this fork
    spent_from_base: DashSet<OutPoint>,

    /// blocks produced on this fork (keyed by hash)
    block_delta: DashMap<BlockHash, Block>,

    /// per-block undo diff created by UtxoProcessor::apply_block
    undo_delta: DashMap<BlockHash, BlockUndo>,
}

impl<DB> ForkDbOverlay<DB>
where
    DB: UtxoStorage + BlockStorage + StorageStats + UndoStorage + Send + Sync + 'static,
{
    pub fn new(base: Arc<DB>, work: u128) -> Self {
        Self {
            base,
            inherited_work: work,
            utxo_delta: DashMap::new(),
            spent_from_base: DashSet::new(),
            block_delta: DashMap::new(),
            undo_delta: DashMap::new(),
        }
    }

    /// Applies **all** buffered changes to the canonical storage.
    ///
    /// The sequence is atomic from the caller’s point of view,
    /// because a write‑lock on the underlying `DB` is held by the
    /// consensus engine while this function runs.
    // TODO: Add some tests for the `ForkDbOverlay::commit` method
    pub async fn commit(self, db: &mut DB) -> Result<(), StryiCoreError> {
        // write blocks first so that UTXO batch refers to known hashes
        for blk in self.block_delta.into_iter().map(|kv| kv.1) {
            db.put_block(&blk).await.map_err(|e| {
                StryiCoreError::storage(
                    StorageLayer::Block,
                    format!("failed put_block during commit: {e:?}"),
                )
            })?;
        }

        // write undo data
        for kv in self.undo_delta.into_iter() {
            let (hash, undo) = kv;
            db.put_block_undo(hash, undo).await.map_err(|e| {
                StryiCoreError::storage(
                    StorageLayer::Undo,
                    format!("failed put_block_undo during commit: {e:?}"),
                )
            })?
        }

        // batch‑insert newly created / overridden UTXOs
        if !self.utxo_delta.is_empty() {
            let puts: Vec<_> = self.utxo_delta.into_iter().collect();
            db.batch_put_utxos(puts).await.map_err(|e| {
                StryiCoreError::storage(
                    StorageLayer::Utxo,
                    format!("failed put_utxos during commit: {e:?}"),
                )
            })?;
        }

        // batch‑remove UTXO that were spent from the base chain
        if !self.spent_from_base.is_empty() {
            let spent: Vec<_> = self.spent_from_base.into_iter().collect();
            db.batch_remove_utxos(spent).await.map_err(|e| {
                StryiCoreError::storage(
                    StorageLayer::Utxo,
                    format!("failed remove_utxos during commit: {e:?}"),
                )
            })?;
        }
        Ok(())
    }

    /// Drops the overlay without touching the canonical storage.
    pub fn abort(self) { /* nothing — `self` drops and DashMap memory is freed */
    }

    /// Returns overlay status of the outpoint:
    /// - Some(Some(utxo))  : present in utxo_delta (created/overridden by this fork)
    /// - Some(None)        : spent in this fork (was in base, now considered absent)
    /// - None              : overlay has no information, caller should query the base
    fn overlay_get_utxo(&self, op: &OutPoint) -> Option<Option<UTXO>> {
        // If there is such an utxo in delta - return as is
        if let Some(u) = self.utxo_delta.get(op) {
            return Some(Some(*u));
        }
        // If this utxo exists in base but spent in *this* fork's state - Some(None)
        if self.spent_from_base.contains(op) {
            return Some(None);
        }
        None
    }
}

impl<DB> UtxoStorage for ForkDbOverlay<DB>
where
    DB: UtxoStorage + BlockStorage + StorageStats + UndoStorage + Send + Sync + 'static,
{
    type StorageError = StryiCoreError;

    fn batch_put_utxos(
        &mut self,
        utxos: Vec<(OutPoint, UTXO)>,
    ) -> BoxFuture<Result<(), Self::StorageError>> {
        // Pure in-memory update; no async work required.
        for (op, utxo) in utxos {
            // Insert the fork-local copy.
            self.utxo_delta.insert(op, utxo);
        }
        Box::pin(ready(Ok(())))
    }

    fn batch_remove_utxos(
        &mut self,
        outpoints: Vec<OutPoint>,
    ) -> BoxFuture<Result<(), Self::StorageError>> {
        // overlay-only update; no async I/O required
        for op in outpoints {
            // If the UTXO was created/overridden in this fork, drop it from delta.
            // Otherwise, mark the original base-chain UTXO as spent in this fork.
            if self.utxo_delta.remove(&op).is_none() {
                self.spent_from_base.insert(op);
            }
        }
        Box::pin(ready(Ok(())))
    }

    fn batch_get_utxos<I>(
        &self,
        outpoints: I,
    ) -> BoxFuture<Result<HashMap<OutPoint, UTXO>, Self::StorageError>>
    where
        I: IntoIterator<Item = OutPoint> + Send,
        I::IntoIter: Send,
    {
        let mut hit: HashMap<OutPoint, UTXO> = HashMap::new();
        let mut miss: Vec<OutPoint> = Vec::new();
        let base = Arc::clone(&self.base);

        // clone outpoints
        let outpoints = outpoints.into_iter().collect::<Vec<_>>().clone();

        Box::pin(async move {
            for outpoint in outpoints {
                // firstly try get utxo from overlay state
                match self.overlay_get_utxo(&outpoint) {
                    // Some(Some(_)) means UTXO really here
                    Some(Some(utxo)) => {
                        hit.insert(outpoint, utxo);
                    }
                    // Some(None) means that UTXO is spent in this fork
                    Some(None) => {
                        // marked as spent in the overlay – treat as missing
                        return Err(StryiCoreError::ConsensusValidationFailed {
                            details: "The UTXO exists in fork's overlay but marked as spent"
                                .to_string(),
                        });
                    }
                    None => miss.push(outpoint),
                }
            }

            // If some utxos are missing after reading them from overlay state - try to get them from base storage
            if !miss.is_empty() {
                // Request all the missing in overlay-storage UTXOs in base storage
                let from_base = base
                    .batch_get_utxos(miss.clone())
                    .await
                    .expect("TODO: Proper error handling ");

                // If the base doesn't have all requested outpoints, it's an error.
                if from_base.len() != miss.len() {
                    return Err(StryiCoreError::ConsensusValidationFailed {
                        details:
                            "The fork's block refers to an unknown outpoint(s) in base storage"
                                .to_string(),
                    });
                }

                // Insert all utxos we found in hit
                for (op, utxo) in from_base {
                    hit.insert(op, utxo);
                }
            }
            Ok(hit)
        })
    }

    fn get_utxos_for_address(
        &self,
        address: AccountAddress,
    ) -> BoxFuture<Result<HashMap<OutPoint, UTXO>, Self::StorageError>> {
        // Clone handles to concurrent maps/sets so they can be moved into `async`.
        let overlay_map = self.utxo_delta.clone();
        let spent_set = self.spent_from_base.clone();
        let base = Arc::clone(&self.base);

        Box::pin(async move {
            // Collect UTXOs created/overridden in this fork
            let mut result: HashMap<OutPoint, UTXO> = HashMap::new();
            for entry in overlay_map.iter() {
                if entry.value().owner == address {
                    result.insert(*entry.key(), *entry.value());
                }
            }

            // UTXOs owned by `address` in the base storage
            let base_map = base
                .get_utxos_for_address(address)
                .await
                .expect("TODO: Better error handling");

            for (op, utxo) in base_map {
                // Skip if fork has already spent this outpoint
                if spent_set.contains(&op) {
                    continue;
                }
                // Skip if fork overrides this outpoint (already in `result`)
                if overlay_map.contains_key(&op) {
                    continue;
                }
                result.insert(op, utxo);
            }

            Ok(result)
        })
    }
}

impl<DB> BlockStorage for ForkDbOverlay<DB>
where
    DB: UtxoStorage + BlockStorage + StorageStats + UndoStorage + Send + Sync + 'static,
{
    type StorageError = StryiCoreError;

    fn put_block(&mut self, block: &Block) -> BoxFuture<Result<(), Self::StorageError>> {
        // overlay-only write – the base DB is untouched
        let cloned = block.clone();
        self.block_delta.insert(block.block_hash(), cloned);
        Box::pin(ready(Ok(())))
    }

    fn batch_get_blocks_by_hashes(
        &self,
        hashes: Vec<BlockHash>,
    ) -> BoxFuture<Result<HashMap<BlockHash, Block>, Self::StorageError>> {
        let delta = self.block_delta.clone();
        let base = Arc::clone(&self.base);

        Box::pin(async move {
            let mut hit = HashMap::with_capacity(hashes.len());
            let mut miss = Vec::new();

            for h in hashes {
                if let Some(b) = delta.get(&h) {
                    hit.insert(h, b.clone());
                } else {
                    miss.push(h);
                }
            }

            if !miss.is_empty() {
                let from_base = base
                    .batch_get_blocks_by_hashes(miss.clone())
                    .await
                    .expect("TODO: Better error handling");
                if from_base.len() != miss.len() {
                    return Err(StryiCoreError::ConsensusValidationFailed {
                        details: "some hashes are missing in base storage".into(),
                    });
                }
                hit.extend(from_base);
            }
            Ok(hit)
        })
    }

    fn batch_get_blocks_by_heights<I>(
        &self,
        heights: I,
    ) -> BoxFuture<Result<HashMap<u64, Block>, Self::StorageError>>
    where
        I: IntoIterator<Item = u64> + Send + Clone,
        I::IntoIter: Send,
    {
        let delta = self.block_delta.clone();
        let base = Arc::clone(&self.base);
        let requested: Vec<u64> = heights.into_iter().collect();

        Box::pin(async move {
            let mut hit = HashMap::with_capacity(requested.len());
            let mut miss = Vec::new();

            // scan overlay once
            for blk in delta.iter() {
                let h = blk.value().header.height;
                if requested.contains(&h) {
                    hit.insert(h, blk.value().clone());
                }
            }
            // anything not satisfied goes to base
            for &h in &requested {
                if !hit.contains_key(&h) {
                    miss.push(h);
                }
            }
            if !miss.is_empty() {
                let from_base = base
                    .batch_get_blocks_by_heights(miss.clone())
                    .await
                    .expect("TODO: Better error handling");
                if from_base.len() != miss.len() {
                    return Err(StryiCoreError::ConsensusValidationFailed {
                        details: "some heights are missing in base storage".into(),
                    });
                }
                hit.extend(from_base);
            }
            Ok(hit)
        })
    }

    fn blocks_range(
        &self,
        range: RangeInclusive<usize>,
    ) -> BoxFuture<Result<HashMap<u64, Block>, Self::StorageError>> {
        let delta = self.block_delta.clone();
        let base = Arc::clone(&self.base);

        let heights: Vec<u64> = range.into_iter().map(|k| k as u64).collect();

        Box::pin(async move {
            // reuse batch_get_blocks_by_heights on the base,
            // but merge overlay priority manually
            let mut result: HashMap<u64, Block> = HashMap::new();

            // overlay first
            for blk in delta.iter() {
                let h = blk.value().header.height;
                if range.contains(&(h as usize)) {
                    result.insert(h, blk.value().clone());
                }
            }

            // fill gaps from base
            let mut gaps = Vec::new();
            for h in &heights {
                if !result.contains_key(h) {
                    gaps.push(*h);
                }
            }

            if !gaps.is_empty() {
                let from_base = base
                    .batch_get_blocks_by_heights(gaps.clone())
                    .await
                    .expect("TODO: Better error handling");
                if from_base.len() != gaps.len() {
                    return Err(StryiCoreError::ConsensusValidationFailed {
                        details: "range contains missing blocks".into(),
                    });
                }
                result.extend(from_base);
            }
            Ok(result)
        })
    }

    fn block_exists(&self, hash: BlockHash) -> BoxFuture<Result<bool, Self::StorageError>> {
        let exists = self.block_delta.contains_key(&hash);
        if exists {
            return Box::pin(ready(Ok(true)));
        }

        let base = Arc::clone(&self.base);
        Box::pin(async move {
            Ok(base
                .block_exists(hash)
                .await
                .expect("TODO: Better error handling"))
        })
    }
}

impl<DB> StorageStats for ForkDbOverlay<DB>
where
    DB: UtxoStorage + BlockStorage + StorageStats + UndoStorage + Send + Sync + 'static,
{
    type StorageError = StryiCoreError;

    fn tip(&self) -> BoxFuture<Result<(u64, BlockHash), Self::StorageError>> {
        let delta = self.block_delta.clone();
        Box::pin(async move {
            let block = delta
                .iter()
                .by_ref()
                .max_by_key(|b| b.header.height)
                .ok_or_else(|| StryiCoreError::ConsensusValidationFailed {
                    details: "overlay is empty".into(),
                })?;

            Ok((block.header.height, block.block_hash()))
        })
    }

    fn last_updated(&self) -> BoxFuture<Result<u64, Self::StorageError>> {
        let delta = self.block_delta.clone();
        Box::pin(async move {
            let ts = delta
                .iter()
                .by_ref()
                .map(|b| b.header.timestamp)
                .max()
                .ok_or_else(|| StryiCoreError::ConsensusValidationFailed {
                    details: "overlay is empty".into(),
                })?;
            Ok(ts)
        })
    }

    fn block_count(&self) -> BoxFuture<Result<u64, Self::StorageError>> {
        let delta_len = self.block_delta.len() as u64;
        let base = Arc::clone(&self.base);
        let delta = self.block_delta.clone(); // need hash set of overlay hashes

        Box::pin(async move {
            // total blocks in base (async)
            let base_cnt = base
                .block_count()
                .await
                .expect("TODO: Better error handling");

            // minus those overridden in overlay
            let mut overridden = 0u64;
            for kv in delta.iter() {
                if base
                    .block_exists(*kv.key())
                    .await
                    .expect("TODO: Better error handling")
                {
                    overridden += 1;
                }
            }

            Ok(base_cnt + delta_len - overridden)
        })
    }

    fn chain_difficulty(&self) -> BoxFuture<'_, Result<u128, Self::StorageError>> {
        let delta = self.block_delta.clone();
        let base_work = self.inherited_work;
        let base = Arc::clone(&self.base);
        Box::pin(async move {
            // start value
            let mut sum = base_work;

            // adjust sum per each overlay block
            for kv in delta.iter() {
                let pow = 1u128 << kv.value().header.difficulty_bits;
                // if base already has this hash, it’s being replaced - remove old work
                if base
                    .block_exists(*kv.key())
                    .await
                    .expect("TODO: Better error handling")
                {
                    sum = sum.wrapping_sub(pow);
                }
                // add overlay block’s work
                sum = sum.wrapping_add(pow);
            }
            Ok(sum)
        })
    }
}

impl<DB> UndoStorage for ForkDbOverlay<DB>
where
    DB: UtxoStorage + BlockStorage + StorageStats + UndoStorage + Send + Sync + 'static,
{
    type StorageError = StryiCoreError;

    /// Inserts or overwrites the undo-record in the in-memory map.
    fn put_block_undo(
        &self,
        hash: BlockHash,
        undo: BlockUndo,
    ) -> BoxFuture<'_, Result<(), Self::StorageError>> {
        self.undo_delta.insert(hash, undo);
        Box::pin(ready(Ok(())))
    }

    /// Reads undo-data from the overlay. Does **not** fall through to the base
    /// storage – caller must query the base separately if needed.
    fn get_block_undo(
        &self,
        hash: BlockHash,
    ) -> BoxFuture<'_, Result<Option<BlockUndo>, Self::StorageError>> {
        let res = self.undo_delta.get(&hash).map(|u| u.clone());
        Box::pin(ready(Ok(res)))
    }

    /// Removes the undo-record from the overlay (no-op if absent).
    fn delete_block_undo(&self, hash: BlockHash) -> BoxFuture<'_, Result<(), Self::StorageError>> {
        self.undo_delta.remove(&hash);
        Box::pin(ready(Ok(())))
    }
}
