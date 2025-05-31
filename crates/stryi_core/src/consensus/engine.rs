use std::sync::Arc;
use tracing::{debug, info};
use futures::future::BoxFuture;
use crate::{
    block::Block,
    consensus::{ConsensusEngine, ConsensusRules},
};
use crate::consensus::ConsensusOnBlockVerdict;
use crate::consensus::index::ChainIndex;
use crate::consensus::validator::BlockValidator;
use crate::error::{StorageLayer, StryiCoreError};
use crate::forktree::ForkStorage;
use crate::storage::{UtxoStorage, BlockStorage, StorageStats};
use crate::transactions::UtxoProcessor;

#[cfg(test)]
use crate::storage::StryiInMemoryStorage;

// StryiConsensusEngine is responsible for validating and processing blocks according to the consensus rules.
///
/// It is generic over two types parameters:
/// - `DB` (database) that implements the `UtxoStorage`, `BlockStorage` and `StorageStats` traits. This enables the engine
/// to work with any storage backend that conforms to the interface (e.g. InMemoryUtxoStorage, StryiStorage, etc.).
/// - `FS` (Stands for Forks Storage) that implements `ForkStorage` trait. It allows engine use different backends
///  for storing and maintaining forks tree.
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


    /// Shared handle to the underlying storage.
    ///
    /// The engine owns only an `Arc`, so the same `DB` instance can be reused
    /// by RPC handlers, the mempool, etc., without extra locking overhead.
    pub(crate) db: Arc<DB>,

    /// Lightweight in-memory index of the *active* chain.
    ///
    /// Maps each block hash to its height and cumulative work, giving O(1)
    /// ancestor look-ups and fast fork-choice comparisons.
    pub(crate) chain_index: ChainIndex,

    /// Repository of all competing side branches.
    ///
    /// Stores blocks that are valid but have **not** yet won fork-choice.
    /// Each entry remembers cumulative work and the common ancestor,
    /// allowing quick reorganisation if this branch becomes "better" than main one.
    pub(crate) forks: FS,
}

impl<DB: UtxoStorage + BlockStorage + StorageStats, FS: ForkStorage> StryiConsensusEngine<DB, FS> {

    /// Creates a fully wired consensus engine.
    ///
    /// * Scans the current best chain in `db` to build `ChainIndex`.
    /// * Leaves `forks` empty; side branches appear as `on_block` is called.
    pub async fn new(
        rules: ConsensusRules,
        block_validator: BlockValidator,
        utxo_processor: UtxoProcessor,
        db: Arc<DB>,
        forks: FS,
    ) -> Result<Self, StryiCoreError> {

        let chain_index = Self::build_chain_index(&*db).await?;

        Ok(Self {
            rules,
            block_validator,
            utxo_processor,
            db,
            chain_index,
            forks,
        })
    }



    /// Walks from the stored tip back to genesis and fills `ChainIndex`.
    /// May return an error if chain refers to unknown block.
    async fn build_chain_index(db: &DB) -> Result<ChainIndex, StryiCoreError> {

        debug!("Starting collecting chain index");
        
        let mut index = ChainIndex::default();


        // Get latest block.
        let mut cursor_hash = db.tip().await
            // Map error if any
            .map_err(|e|
                StryiCoreError::storage(
                    StorageLayer::Block,
                    format!("Cannot get tip block from storage: {e:#?}")))?
            // (block_height, block_hash)
            .1;


        // acc
        let mut cumulative_work: u128 = 0;


        // Iterate over blocks one by one
        loop {
            match db.get_block_by_hash(cursor_hash).await {
                Ok(Some(block)) => {
                    cumulative_work += 1u128 << block.header.difficulty_bits;

                    // Add this block in index with specified cumulative work.
                    index.insert(&block, cumulative_work);

                    // reached genesis - success
                    if block.header.height == 0 {
                        break;
                    }

                    cursor_hash = block.header.previous_block_hash;

                },

                // Block not found -> error
                Ok(None) => return Err(
                    StryiCoreError::storage(
                        StorageLayer::Block,
                        format!("Cannot find block {} in persistent storage to build chain index", cursor_hash)
                    )
                ),

                // If got any error while traversing blocks -> return.
                Err(e) => return Err(
                    StryiCoreError::storage(
                        StorageLayer::Block,
                        format!("{e:?}")
                    )
                )
            };
        }

        Ok(index)
    }

    /// Computes the cumulative chain work for a given slice of blocks.
    ///
    /// For each block, the work is defined as 2^(difficulty_bits).
    /// The total chain work is the sum of these values.
    /// This metric is used in chain selection algorithms to determine which fork is "heavier."
    // TODO: New trait + generic parameter for better computation of chain's difficulty?
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
    type Error = StryiCoreError;

    fn on_block(&mut self, block: Block) -> BoxFuture<Result<ConsensusOnBlockVerdict, Self::Error>> {

        todo!()
    }
}