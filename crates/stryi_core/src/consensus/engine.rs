use std::marker::PhantomData;
use futures::future::BoxFuture;
use crate::{
    block::Block,
    consensus::{ConsensusEngine, ConsensusRules},
};
use crate::consensus::ConsensusOnBlockVerdict;
use crate::consensus::error::ConsensusEngineError;
use crate::consensus::validator::BlockValidator;
use crate::forktree::ForkStorage;
use crate::storage::{UtxoStorage, BlockStorage, StorageStats};
use crate::transactions::UtxoProcessor;

#[cfg(test)]
use crate::storage::StryiInMemoryStorage;

/// StryiConsensusEngine is responsible for validating and processing blocks according to the consensus rules.
///
/// It is generic over two types parameters:
/// - `DB` (database) that implements the `UtxoStorage`, `BlockStorage` and `StorageStats` traits. This enables the engine
/// to work with any storage backend that conforms to the interface (e.g. InMemoryUtxoStorage, StryiStorage, etc.).
/// - `FS` (Stands for Forks Storage) that implements `ForkStorage` trait. It allows engine use different backends
///  for storing and maintaining forks tree.
///
/// The `_phantom` fields are a PhantomData markers used to hold the generic type parameters `DB` and `FS` without storing an actual instance.
pub struct StryiConsensusEngine<DB, FS> where
    DB: UtxoStorage + BlockStorage + StorageStats,
    FS: ForkStorage {
    /// Consensus rules object defining parameters like current difficulty and adjustment intervals.
    pub(crate) rules: ConsensusRules,

    /// Block validator used to verify block-level properties such as proof-of-work, merkle root correctness,
    /// coinbase placement, transaction dependencies and ordering
    pub(crate) block_validator: BlockValidator,

    /// UTXO processor that applies transactions within a block to update the UTXO set.
    // TODO: consider renaming it later, maybe in TransactionsProcessor? Current name is a little weird
    pub(crate) utxo_processor: UtxoProcessor,

    /// PhantomData marker to associate the generic storage type DB with this engine.
    _db_phantom: PhantomData<DB>,
    /// PhantomData marker to associate the generic storage type FS with this engine.
    _fs_phantom: PhantomData<FS>
}

impl<DB: UtxoStorage + BlockStorage + StorageStats, FS: ForkStorage> StryiConsensusEngine<DB, FS> {
    /// Constructs a new StryiConsensusEngine using the provided consensus rules.
    ///
    /// The engine is initialized with:
    /// - A copy of the consensus rules.
    /// - A new BlockValidator instance (initialized with the current difficulty).
    /// - A new UtxoProcessor.
    /// - A PhantomData marker for the DB type.
    pub fn new(rules: ConsensusRules) -> Self {
        Self {
            rules: rules.clone(),
            block_validator: BlockValidator::new(rules),
            utxo_processor: UtxoProcessor::new(),
            _db_phantom: PhantomData,
            _fs_phantom: PhantomData
        }
    }

    /// Computes the cumulative chain work for a given slice of blocks.
    ///
    /// For each block, the work is defined as 2^(difficulty_bits).
    /// The total chain work is the sum of these values.
    /// This metric is used in chain selection algorithms to determine which fork is "heavier."
    // TODO: New trait for better computation of chain's difficulty? 
    pub fn compute_chain_difficulty(&self, chain: &[Block]) -> u128 {
        let mut total = 0u128;
        for block in chain {
            let bits = block.header.difficulty_bits;
            total = total.saturating_add(1u128 << bits);
        }
        total
    }
}


impl<DB : UtxoStorage + BlockStorage + StorageStats, FS : ForkStorage> ConsensusEngine for StryiConsensusEngine<DB, FS> {
    type Error = ConsensusEngineError;

    fn on_block(&mut self, block: Block) -> BoxFuture<'static, Result<ConsensusOnBlockVerdict, Self::Error>> {
        todo!()
    }
}