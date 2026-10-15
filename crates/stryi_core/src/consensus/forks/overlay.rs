use crate::BlockUndo;
use crate::address::AccountAddress;
use crate::block::{Block, BlockHash};
use crate::difficulty::HashThreshold;
use crate::storage::{
    BlockIndexData, BlockStorage, ConfirmedTransaction, StorageError, StorageRead, UtxoStorage,
    UtxoWrite, verify_genesis_work_shape,
};
use crate::transactions::{OutPoint, TransactionHash, UTXO, UtxoProcessor};
use std::collections::{HashMap, HashSet};

/// In-memory overlay on top of a storage snapshot for validating and preparing a fork.
///
/// Rewinds blocks from canonical tip down to `common_ancestor`, replays fork blocks,
/// and converts the resulting state into [`ForkDbDelta`] via `into_delta`.
/// Canonical heights and txs above `common_ancestor` are masked so difficulty/header
/// validation on the fork won't accidentally read blocks from the old main branch.
pub struct ForkDbOverlay<'a, DB: StorageRead> {
    base: &'a DB,
    common_ancestor: (u64, BlockHash),
    expected_head: (u64, BlockHash),
    rewind_tip: (u64, BlockHash),
    branch_tip: (u64, BlockHash),
    utxos: HashMap<OutPoint, UTXO>,
    /// Can also contain outpoints created and spent inside the fork itself (not present in `base`).
    removed_utxos: HashSet<OutPoint>,
    blocks: HashMap<BlockHash, (Block, BlockUndo)>,
    heights: HashMap<u64, BlockHash>,
    detached: Vec<(u64, BlockHash)>,
}

/// Prepared reorg batch produced by [`ForkDbOverlay::into_delta`].
pub struct ForkDbDelta {
    pub(crate) expected_head: (u64, BlockHash),
    pub(crate) common_ancestor: BlockHash,
    pub(crate) detached: Vec<(u64, BlockHash)>,
    /// Fork blocks in parent-first order along with their undo data.
    pub(crate) blocks: Vec<(Block, BlockUndo)>,
    pub(crate) utxos: Vec<(OutPoint, UTXO)>,
    pub(crate) removed_utxos: Vec<OutPoint>,
}

impl<'a, DB: StorageRead> ForkDbOverlay<'a, DB> {
    /// Creates a new overlay rooted at `ancestor`.
    /// Call `rewind_block` from current tip down to `ancestor` before replaying fork blocks.
    pub fn new(base: &'a DB, ancestor: BlockHash) -> Result<Self, StorageError> {
        let block = base
            .get_block_by_hash(ancestor)?
            .ok_or(StorageError::BlockNotFound(ancestor))?;
        let expected_head = base.tip()?;

        if block.block_hash() != ancestor {
            return Err(StorageError::Corruption(
                "Fork ancestor body does not match its key".into(),
            ));
        }

        if block.header.height > expected_head.0
            || base.get_block_hash_by_height(block.header.height)? != Some(ancestor)
        {
            return Err(StorageError::InvalidTransition(
                "Fork ancestor is outside the canonical chain".into(),
            ));
        }

        let common_ancestor = (block.header.height, ancestor);

        Ok(Self {
            base,
            common_ancestor,
            expected_head,
            rewind_tip: expected_head,
            branch_tip: common_ancestor,
            utxos: HashMap::new(),
            removed_utxos: HashSet::new(),
            blocks: HashMap::new(),
            heights: HashMap::new(),
            detached: Vec::new(),
        })
    }

    /// Rewind one canonical block using undo from this overlay's base snapshot.
    /// This only changes the overlay. Discard it on any error.
    pub fn rewind_block(&mut self, processor: &UtxoProcessor) -> Result<(), StorageError> {
        if !self.blocks.is_empty() || self.rewind_tip.0 <= self.common_ancestor.0 {
            return Err(StorageError::InvalidTransition(
                "Cannot rewind past the fork ancestor or after replay".into(),
            ));
        }

        let (height, hash) = self.rewind_tip;
        let block = self
            .base
            .get_block_by_hash(hash)?
            .ok_or_else(|| StorageError::Corruption(format!("Canonical block {hash} is absent")))?;

        if block.block_hash() != hash
            || block.header.height != height
            || self.base.get_block_hash_by_height(height)? != Some(hash)
        {
            return Err(StorageError::Corruption(
                "Canonical ancestry and height mappings disagree".into(),
            ));
        }

        let undo = self
            .base
            .get_block_undo(hash)?
            .ok_or_else(|| StorageError::Corruption(format!("Canonical undo {hash} is absent")))?;

        processor
            .rewind_block(undo, self)
            .map_err(|error| match error {
                StorageError::UtxoNotFound(outpoint) => {
                    StorageError::Corruption(format!("Undo references missing UTXO {outpoint:?}"))
                }
                other => other,
            })?;

        self.detached.push((height, hash));
        self.rewind_tip = (height - 1, block.header.previous_block_hash);

        Ok(())
    }

    /// Applies a validated fork block on top of the rewound ancestor state (in parent-first order).
    pub fn replay_block(&mut self, block: &Block) -> Result<(), StorageError> {
        verify_genesis_work_shape(block)
            .map_err(|error| StorageError::InvalidTransition(error.to_string()))?;

        if self.rewind_tip != self.common_ancestor
            || block.header.is_genesis()
            || block.header.hash_threshold == HashThreshold::ZERO
            || block.header.previous_block_hash != self.branch_tip.1
            || self.branch_tip.0.checked_add(1) != Some(block.header.height)
        {
            return Err(StorageError::InvalidTransition(
                "Fork replay must extend the rewound ancestor or its replacement prefix".into(),
            ));
        }

        let undo = UtxoProcessor::new().apply_block(block, self)?;
        let hash = block.block_hash();
        self.blocks.insert(hash, (block.clone(), undo));
        self.heights.insert(block.header.height, hash);
        self.branch_tip = (block.header.height, hash);

        Ok(())
    }

    /// Consume a complete replacement into net changes relative to the base tip.
    /// A local put wins over a removal marker. Outputs created and then spent
    /// only inside the overlay must not become deletions from canonical storage.
    pub fn into_delta(self) -> Result<ForkDbDelta, StorageError> {
        if self.rewind_tip != self.common_ancestor || self.blocks.is_empty() {
            return Err(StorageError::InvalidTransition(
                "Fork delta requires a complete rewind and a nonempty replacement".into(),
            ));
        }

        let mut removed_utxos = Vec::new();
        for outpoint in self.removed_utxos {
            if !self.utxos.contains_key(&outpoint) && self.base.get_utxo(outpoint)?.is_some() {
                removed_utxos.push(outpoint);
            }
        }

        let mut utxos = Vec::new();
        for (outpoint, utxo) in self.utxos {
            if self.base.get_utxo(outpoint)? != Some(utxo) {
                utxos.push((outpoint, utxo));
            }
        }

        let mut blocks: Vec<_> = self.blocks.into_values().collect();
        blocks.sort_unstable_by_key(|(block, _)| block.header.height);

        Ok(ForkDbDelta {
            expected_head: self.expected_head,
            common_ancestor: self.common_ancestor.1,
            detached: self.detached,
            blocks,
            utxos,
            removed_utxos,
        })
    }
}

impl<DB: StorageRead> UtxoStorage for ForkDbOverlay<'_, DB> {
    fn get_utxo(&self, outpoint: OutPoint) -> Result<Option<UTXO>, StorageError> {
        // A restored or recreated output takes precedence over a base removal marker.
        if let Some(utxo) = self.utxos.get(&outpoint) {
            return Ok(Some(*utxo));
        }

        if self.removed_utxos.contains(&outpoint) {
            return Ok(None);
        }

        self.base.get_utxo(outpoint)
    }

    fn get_utxos_for_address(
        &self,
        address: AccountAddress,
    ) -> Result<HashMap<OutPoint, UTXO>, StorageError> {
        let mut result = self.base.get_utxos_for_address(address)?;
        result.retain(|outpoint, _| {
            !self.removed_utxos.contains(outpoint) && !self.utxos.contains_key(outpoint)
        });

        result.extend(
            self.utxos
                .iter()
                .filter(|(_, utxo)| utxo.owner == address)
                .map(|(outpoint, utxo)| (*outpoint, *utxo)),
        );

        Ok(result)
    }
}

impl<DB: StorageRead> BlockStorage for ForkDbOverlay<'_, DB> {
    fn get_block_hash_by_height(&self, height: u64) -> Result<Option<BlockHash>, StorageError> {
        if let Some(hash) = self.heights.get(&height) {
            return Ok(Some(*hash));
        }

        if height > self.common_ancestor.0 {
            return Ok(None);
        }

        self.base.get_block_hash_by_height(height)
    }

    fn get_block_by_hash(&self, hash: BlockHash) -> Result<Option<Block>, StorageError> {
        if let Some((block, _)) = self.blocks.get(&hash) {
            return Ok(Some(block.clone()));
        }

        self.base.get_block_by_hash(hash)
    }

    fn get_block_by_height(&self, height: u64) -> Result<Option<Block>, StorageError> {
        if let Some(hash) = self.heights.get(&height) {
            return self.get_block_by_hash(*hash);
        }

        if height > self.common_ancestor.0 {
            return Ok(None);
        }

        self.base.get_block_by_height(height)
    }

    fn get_block_index(&self, hash: &BlockHash) -> Result<Option<BlockIndexData>, StorageError> {
        self.base.get_block_index(hash)
    }

    fn get_transaction(
        &self,
        hash: &TransactionHash,
    ) -> Result<Option<ConfirmedTransaction>, StorageError> {
        Ok(self
            .base
            .get_transaction(hash)?
            .filter(|record| record.location.block_height <= self.common_ancestor.0))
    }
}

impl<DB: StorageRead> UtxoWrite for ForkDbOverlay<'_, DB> {
    fn batch_put_utxos(&mut self, utxos: Vec<(OutPoint, UTXO)>) -> Result<(), StorageError> {
        self.utxos.extend(utxos);
        Ok(())
    }

    fn batch_remove_utxos(&mut self, outpoints: Vec<OutPoint>) -> Result<(), StorageError> {
        for outpoint in &outpoints {
            if self.get_utxo(*outpoint)?.is_none() {
                return Err(StorageError::UtxoNotFound(*outpoint));
            }
        }

        for outpoint in outpoints {
            self.utxos.remove(&outpoint);
            self.removed_utxos.insert(outpoint);
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consensus::ConsensusConsts;
    use crate::difficulty::{DifficultyCalc, HashThreshold, PowParams};
    use crate::storage::{ChainTransition, FullNodeStorage, StorageReader};
    use crate::tests::make_test_genesis;

    #[test]
    fn asert_uses_parent_branch() {
        let pow = PowParams {
            target_spacing_secs: 3600,
            half_life_secs: 3600,
            initial_hash_threshold: HashThreshold::from(1_000_000u64),
            max_hash_threshold: HashThreshold::from(10_000_000u64),
        };
        let consts = ConsensusConsts {
            pow,
            ..ConsensusConsts::default()
        };
        let (genesis, db) = make_test_genesis(&[(AccountAddress::new(&[1; 20]), 1)], consts);
        let calc = DifficultyCalc::new(pow).unwrap();
        let initial = pow.initial_hash_threshold;

        assert_eq!(
            calc.next_hash_threshold(&db.read().unwrap(), genesis.block_hash())
                .unwrap(),
            initial
        );

        let first = Block::new(vec![], genesis.block_hash(), 1, initial, 1000, 1);
        db.apply_transition(ChainTransition::Append { block: &first })
            .unwrap();
        assert_eq!(
            calc.next_hash_threshold(&db.read().unwrap(), first.block_hash())
                .unwrap(),
            initial
        );

        let slow = Block::new(vec![], first.block_hash(), 2, initial, 8200, 1);
        db.apply_transition(ChainTransition::Append { block: &slow })
            .unwrap();
        assert_eq!(
            calc.next_hash_threshold(&db.read().unwrap(), slow.block_hash())
                .unwrap(),
            HashThreshold::from(2_000_000u64)
        );

        let view = db.read().unwrap();
        let mut shared = ForkDbOverlay::new(&view, first.block_hash()).unwrap();
        shared.rewind_block(&UtxoProcessor::new()).unwrap();
        let fast = Block::new(vec![], first.block_hash(), 2, initial, 2800, 1);
        shared.replay_block(&fast).unwrap();
        assert_eq!(
            calc.next_hash_threshold(&shared, fast.block_hash())
                .unwrap(),
            HashThreshold::from(707_046u64)
        );

        let mut from_genesis = ForkDbOverlay::new(&view, genesis.block_hash()).unwrap();
        from_genesis.rewind_block(&UtxoProcessor::new()).unwrap();
        from_genesis.rewind_block(&UtxoProcessor::new()).unwrap();
        let fork_first = Block::new(vec![], genesis.block_hash(), 1, initial, 100_000, 1);
        from_genesis.replay_block(&fork_first).unwrap();
        assert_eq!(
            calc.next_hash_threshold(&from_genesis, fork_first.block_hash())
                .unwrap(),
            initial
        );

        let fork_second = Block::new(vec![], fork_first.block_hash(), 2, initial, 103_600, 1);
        from_genesis.replay_block(&fork_second).unwrap();
        assert_eq!(
            calc.next_hash_threshold(&from_genesis, fork_second.block_hash())
                .unwrap(),
            initial
        );
    }
}
