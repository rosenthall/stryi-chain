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
use std::ops::RangeInclusive;

/// In-memory fork view over canonical storage.
///
/// Reads fall through to `base` unless this overlay has a local override.
/// All writes stay local until the overlay is converted into a delta and committed.
pub struct ForkDbOverlay<'a, DB>
where
    DB: UtxoStorage + BlockStorage + StorageStats + UndoStorage + Send + Sync + 'static,
{
    /// Canonical storage snapshot used for reads.
    base: &'a DB,

    /// Cumulative work up to and including the fork point.
    inherited_work: u128,

    /// Fork-local UTXO writes and overrides.
    utxo_delta: DashMap<OutPoint, UTXO>,

    /// Base UTXOs that are considered spent on this fork.
    spent_from_base: DashSet<OutPoint>,

    /// Fork-local blocks keyed by hash.
    block_delta: DashMap<BlockHash, Block>,

    /// Fork-local undo records keyed by block hash.
    undo_delta: DashMap<BlockHash, BlockUndo>,
}

/// Owned changes extracted from a `ForkDbOverlay`.
///
/// This type has no borrow of the base DB, so it can be safely committed
/// into the canonical storage later.
pub struct ForkDbDelta {
    /// Fork-local UTXO writes and overrides.
    utxo_delta: HashMap<OutPoint, UTXO>,

    /// Base UTXOs that must be removed from canonical state.
    spent_from_base: Vec<OutPoint>,

    /// Blocks that must be inserted into canonical storage.
    block_delta: Vec<Block>,

    /// Undo records that must be inserted into canonical storage.
    undo_delta: Vec<(BlockHash, BlockUndo)>,
}

impl ForkDbDelta {
    pub async fn commit<DB>(self, db: &mut DB) -> Result<(), StryiCoreError>
    where
        DB: UtxoStorage + BlockStorage + UndoStorage + Send + Sync + 'static,
    {
        for block in self.block_delta {
            db.put_block(&block).await.map_err(|e| {
                StryiCoreError::storage(
                    StorageLayer::Block,
                    format!("failed put_block during commit: {e}"),
                )
            })?;
        }

        for (hash, undo) in self.undo_delta {
            db.put_block_undo(hash, undo).await.map_err(|e| {
                StryiCoreError::storage(
                    StorageLayer::Undo,
                    format!("failed put_block_undo during commit: {e}"),
                )
            })?;
        }

        if !self.utxo_delta.is_empty() {
            db.batch_put_utxos(self.utxo_delta.into_iter().collect())
                .await
                .map_err(|e| {
                    StryiCoreError::storage(
                        StorageLayer::Utxo,
                        format!("failed put_utxos during commit: {e}"),
                    )
                })?;
        }

        if !self.spent_from_base.is_empty() {
            db.batch_remove_utxos(self.spent_from_base)
                .await
                .map_err(|e| {
                    StryiCoreError::storage(
                        StorageLayer::Utxo,
                        format!("failed remove_utxos during commit: {e}"),
                    )
                })?;
        }

        Ok(())
    }
}

impl<'a, DB> ForkDbOverlay<'a, DB>
where
    DB: UtxoStorage + BlockStorage + StorageStats + UndoStorage + Send + Sync + 'static,
{
    pub fn new(base: &'a DB, work: u128) -> Self {
        Self {
            base,
            inherited_work: work,
            utxo_delta: DashMap::new(),
            spent_from_base: DashSet::new(),
            block_delta: DashMap::new(),
            undo_delta: DashMap::new(),
        }
    }

    pub fn into_delta(self) -> ForkDbDelta {
        ForkDbDelta {
            utxo_delta: self.utxo_delta.into_iter().collect(),
            spent_from_base: self.spent_from_base.into_iter().collect(),
            block_delta: self.block_delta.into_iter().map(|kv| kv.1).collect(),
            undo_delta: self.undo_delta.into_iter().collect(),
        }
    }

    fn overlay_get_utxo(&self, op: &OutPoint) -> Option<Option<UTXO>> {
        if let Some(u) = self.utxo_delta.get(op) {
            return Some(Some(*u));
        }

        if self.spent_from_base.contains(op) {
            return Some(None);
        }

        None
    }
}

impl<'a, DB> UtxoStorage for ForkDbOverlay<'a, DB>
where
    DB: UtxoStorage + BlockStorage + StorageStats + UndoStorage + Send + Sync + 'static,
{
    type StorageError = StryiCoreError;

    fn batch_put_utxos(
        &mut self,
        utxos: Vec<(OutPoint, UTXO)>,
    ) -> BoxFuture<'_, Result<(), Self::StorageError>> {
        for (op, utxo) in utxos {
            self.utxo_delta.insert(op, utxo);
        }

        Box::pin(ready(Ok(())))
    }

    fn batch_remove_utxos(
        &mut self,
        outpoints: Vec<OutPoint>,
    ) -> BoxFuture<'_, Result<(), Self::StorageError>> {
        for op in outpoints {
            if self.utxo_delta.remove(&op).is_none() {
                self.spent_from_base.insert(op);
            }
        }

        Box::pin(ready(Ok(())))
    }

    fn batch_get_utxos<I>(
        &self,
        outpoints: I,
    ) -> BoxFuture<'_, Result<HashMap<OutPoint, UTXO>, Self::StorageError>>
    where
        I: IntoIterator<Item = OutPoint> + Send,
        I::IntoIter: Send,
    {
        let base = self.base;
        let outpoints: Vec<_> = outpoints.into_iter().collect();

        Box::pin(async move {
            let mut hit = HashMap::with_capacity(outpoints.len());
            let mut miss = Vec::new();

            for outpoint in outpoints {
                match self.overlay_get_utxo(&outpoint) {
                    Some(Some(utxo)) => {
                        hit.insert(outpoint, utxo);
                    }
                    Some(None) => {
                        return Err(StryiCoreError::TxMissingUtxo {
                            txid: outpoint.txid,
                            vout: outpoint.vout,
                        });
                    }
                    None => miss.push(outpoint),
                }
            }

            if !miss.is_empty() {
                let from_base = base
                    .batch_get_utxos(miss)
                    .await
                    .map_err(|e| StryiCoreError::storage(StorageLayer::Utxo, e.to_string()))?;

                hit.extend(from_base);
            }

            Ok(hit)
        })
    }

    fn get_utxos_for_address(
        &self,
        address: AccountAddress,
    ) -> BoxFuture<'_, Result<HashMap<OutPoint, UTXO>, Self::StorageError>> {
        let overlay_map = self.utxo_delta.clone();
        let spent_set = self.spent_from_base.clone();
        let base = self.base;

        Box::pin(async move {
            let mut result = HashMap::new();

            for entry in overlay_map.iter() {
                if entry.value().owner == address {
                    result.insert(*entry.key(), *entry.value());
                }
            }

            let base_map = base
                .get_utxos_for_address(address)
                .await
                .map_err(|e| StryiCoreError::storage(StorageLayer::Utxo, e.to_string()))?;

            for (op, utxo) in base_map {
                if spent_set.contains(&op) {
                    continue;
                }

                if overlay_map.contains_key(&op) {
                    continue;
                }

                result.insert(op, utxo);
            }

            Ok(result)
        })
    }
}

impl<'a, DB> BlockStorage for ForkDbOverlay<'a, DB>
where
    DB: UtxoStorage + BlockStorage + StorageStats + UndoStorage + Send + Sync + 'static,
{
    type StorageError = StryiCoreError;

    fn put_block(&mut self, block: &Block) -> BoxFuture<'_, Result<(), Self::StorageError>> {
        self.block_delta.insert(block.block_hash(), block.clone());
        Box::pin(ready(Ok(())))
    }

    fn batch_get_blocks_by_hashes(
        &self,
        hashes: Vec<BlockHash>,
    ) -> BoxFuture<'_, Result<HashMap<BlockHash, Block>, Self::StorageError>> {
        let delta = self.block_delta.clone();
        let base = self.base;

        Box::pin(async move {
            let mut hit = HashMap::with_capacity(hashes.len());
            let mut miss = Vec::new();

            for hash in hashes {
                if let Some(block) = delta.get(&hash) {
                    hit.insert(hash, block.clone());
                } else {
                    miss.push(hash);
                }
            }

            if !miss.is_empty() {
                let from_base = base
                    .batch_get_blocks_by_hashes(miss.clone())
                    .await
                    .map_err(|e| StryiCoreError::storage(StorageLayer::Block, e.to_string()))?;

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
    ) -> BoxFuture<'_, Result<HashMap<u64, Block>, Self::StorageError>>
    where
        I: IntoIterator<Item = u64> + Send + Clone,
        I::IntoIter: Send,
    {
        let delta = self.block_delta.clone();
        let base = self.base;
        let requested: Vec<u64> = heights.into_iter().collect();

        Box::pin(async move {
            let mut hit = HashMap::with_capacity(requested.len());
            let mut miss = Vec::new();

            for blk in delta.iter() {
                let height = blk.value().header.height;
                if requested.contains(&height) {
                    hit.insert(height, blk.value().clone());
                }
            }

            for &height in &requested {
                if !hit.contains_key(&height) {
                    miss.push(height);
                }
            }

            if !miss.is_empty() {
                let from_base = base
                    .batch_get_blocks_by_heights(miss.clone())
                    .await
                    .map_err(|e| StryiCoreError::storage(StorageLayer::Block, e.to_string()))?;

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
    ) -> BoxFuture<'_, Result<HashMap<u64, Block>, Self::StorageError>> {
        let delta = self.block_delta.clone();
        let base = self.base;
        let start = *range.start();
        let end = *range.end();
        let heights: Vec<u64> = (start..=end).map(|n| n as u64).collect();

        Box::pin(async move {
            let mut result = HashMap::new();

            for blk in delta.iter() {
                let height = blk.value().header.height;
                if (start..=end).contains(&(height as usize)) {
                    result.insert(height, blk.value().clone());
                }
            }

            let mut gaps = Vec::new();
            for height in &heights {
                if !result.contains_key(height) {
                    gaps.push(*height);
                }
            }

            if !gaps.is_empty() {
                let from_base = base
                    .batch_get_blocks_by_heights(gaps.clone())
                    .await
                    .map_err(|e| StryiCoreError::storage(StorageLayer::Block, e.to_string()))?;

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

    fn block_exists(&self, hash: BlockHash) -> BoxFuture<'_, Result<bool, Self::StorageError>> {
        if self.block_delta.contains_key(&hash) {
            return Box::pin(ready(Ok(true)));
        }

        let base = self.base;
        Box::pin(async move {
            base.block_exists(hash)
                .await
                .map_err(|e| StryiCoreError::storage(StorageLayer::Block, e.to_string()))
        })
    }
}

impl<'a, DB> StorageStats for ForkDbOverlay<'a, DB>
where
    DB: UtxoStorage + BlockStorage + StorageStats + UndoStorage + Send + Sync + 'static,
{
    type StorageError = StryiCoreError;

    fn tip(&self) -> BoxFuture<'_, Result<(u64, BlockHash), Self::StorageError>> {
        let delta = self.block_delta.clone();

        Box::pin(async move {
            let block = delta
                .iter()
                .max_by_key(|b| b.value().header.height)
                .ok_or_else(|| StryiCoreError::ConsensusValidationFailed {
                    details: "overlay is empty".into(),
                })?;

            Ok((block.value().header.height, block.value().block_hash()))
        })
    }

    fn last_updated(&self) -> BoxFuture<'_, Result<u64, Self::StorageError>> {
        let delta = self.block_delta.clone();

        Box::pin(async move {
            let ts = delta
                .iter()
                .map(|b| b.value().header.timestamp)
                .max()
                .ok_or_else(|| StryiCoreError::ConsensusValidationFailed {
                    details: "overlay is empty".into(),
                })?;

            Ok(ts)
        })
    }

    fn block_count(&self) -> BoxFuture<'_, Result<u64, Self::StorageError>> {
        let delta_len = self.block_delta.len() as u64;
        let delta = self.block_delta.clone();
        let base = self.base;

        Box::pin(async move {
            let base_cnt = base
                .block_count()
                .await
                .map_err(|e| StryiCoreError::storage(StorageLayer::Stats, e.to_string()))?;

            let mut overridden = 0u64;
            for kv in delta.iter() {
                if base
                    .block_exists(*kv.key())
                    .await
                    .map_err(|e| StryiCoreError::storage(StorageLayer::Block, e.to_string()))?
                {
                    overridden += 1;
                }
            }

            Ok(base_cnt + delta_len - overridden)
        })
    }

    fn chain_difficulty(&self) -> BoxFuture<'_, Result<u128, Self::StorageError>> {
        let delta = self.block_delta.clone();
        let base = self.base;
        let base_work = self.inherited_work;

        Box::pin(async move {
            let mut sum = base_work;

            for kv in delta.iter() {
                let pow = 1u128 << kv.value().header.difficulty_bits;

                if base
                    .block_exists(*kv.key())
                    .await
                    .map_err(|e| StryiCoreError::storage(StorageLayer::Block, e.to_string()))?
                {
                    sum = sum.wrapping_sub(pow);
                }

                sum = sum.wrapping_add(pow);
            }

            Ok(sum)
        })
    }
}

impl<'a, DB> UndoStorage for ForkDbOverlay<'a, DB>
where
    DB: UtxoStorage + BlockStorage + StorageStats + UndoStorage + Send + Sync + 'static,
{
    type StorageError = StryiCoreError;

    fn put_block_undo(
        &self,
        hash: BlockHash,
        undo: BlockUndo,
    ) -> BoxFuture<'_, Result<(), Self::StorageError>> {
        self.undo_delta.insert(hash, undo);
        Box::pin(ready(Ok(())))
    }

    fn get_block_undo(
        &self,
        hash: BlockHash,
    ) -> BoxFuture<'_, Result<Option<BlockUndo>, Self::StorageError>> {
        let res = self.undo_delta.get(&hash).map(|u| u.clone());
        Box::pin(ready(Ok(res)))
    }

    fn delete_block_undo(&self, hash: BlockHash) -> BoxFuture<'_, Result<(), Self::StorageError>> {
        self.undo_delta.remove(&hash);
        Box::pin(ready(Ok(())))
    }
}
